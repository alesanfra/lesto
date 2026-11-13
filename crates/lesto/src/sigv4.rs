//! AWS Signature Version 4 for the OTLP exporters (feature `otel`): the CloudWatch OTLP
//! endpoints (`xray`, `logs`, `monitoring`) take SigV4-signed requests, and with it a Lambda
//! function exports to X-Ray with the credentials of its role and no collector in between.
//!
//! The algorithm, step by step as AWS documents it
//! (<https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv-create-signed-request.html>):
//! a canonical request (method, path, query, signed headers, body hash), the string to sign
//! (algorithm, time, scope, hash of the canonical request), a signing key derived from the secret
//! through four HMACs (date, region, service, `aws4_request`), and the `Authorization` header with
//! the signature. Checked against the official test suite in `tests/fixtures/sigv4`.
//!
//! Credentials come from the environment only (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
//! `AWS_SESSION_TOKEN`), which is where Lambda puts those of the function's role. Instance
//! metadata, container credentials and profiles would need a provider chain with refreshes.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_lc_rs::{digest, hmac};
use http::{HeaderValue, Request};
use opentelemetry_http::{Bytes, HttpClient, HttpError, Response};

/// Access key, secret and (for the temporary credentials of a role) session token.
#[derive(Clone)]
pub(crate) struct Credentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[redacted]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

impl Credentials {
    /// The standard variables, read through `get` (the process environment, or a test's map).
    pub(crate) fn read(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let value = |name: &str| get(name).filter(|v| !v.trim().is_empty());
        Some(Credentials {
            access_key_id: value("AWS_ACCESS_KEY_ID")?,
            secret_access_key: value("AWS_SECRET_ACCESS_KEY")?,
            session_token: value("AWS_SESSION_TOKEN"),
        })
    }
}

/// Which service of which region a signature is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scope {
    pub(crate) service: String,
    pub(crate) region: String,
}

impl Scope {
    /// The scope of an AWS OTLP endpoint, read from its host: `xray.eu-west-1.amazonaws.com` is
    /// `xray` in `eu-west-1`. FIPS hosts (`xray-fips.us-east-1…`) sign as the plain service,
    /// China regions end in `.amazonaws.com.cn`. Anything else is not AWS, and is not signed
    /// unless asked to (`LESTO_OTEL_SIGV4=on`).
    pub(crate) fn of_host(host: &str) -> Option<Scope> {
        let host = host.to_ascii_lowercase();
        let rest = host
            .strip_suffix(".amazonaws.com")
            .or_else(|| host.strip_suffix(".amazonaws.com.cn"))?;
        let (service, region) = rest.split_once('.')?;
        let service = service.strip_suffix("-fips").unwrap_or(service);
        let known = ["xray", "logs", "monitoring"];
        let region_ok = !region.is_empty()
            && region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        (known.contains(&service) && region_ok).then(|| Scope {
            service: service.to_string(),
            region: region.to_string(),
        })
    }
}

/// Headers that are never signed (botocore's list): a proxy may change them in transit.
const UNSIGNED: &[&str] = &["user-agent", "expect", "x-amzn-trace-id"];

/// Signs requests for one scope with one set of credentials.
#[derive(Clone, Debug)]
pub(crate) struct Signer {
    pub(crate) credentials: Credentials,
    pub(crate) scope: Scope,
}

/// What a signature adds to a request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Signed {
    pub(crate) amz_date: String,
    pub(crate) authorization: String,
}

impl Signer {
    /// Sign `request` as sent at `now`: adds `x-amz-date`, `x-amz-security-token` (temporary
    /// credentials) and `authorization`. Every header already on the request is signed, plus
    /// `host`, computed as the HTTP client will send it.
    pub(crate) fn sign(&self, request: &mut Request<Bytes>, now: SystemTime) -> Result<(), String> {
        let host = host_header(request.uri()).ok_or("the request URI has no host")?;
        let mut headers = Vec::with_capacity(request.headers().len() + 3);
        for (name, value) in request.headers() {
            // Left unsigned as the AWS SDKs leave them: proxies rewrite these on the way.
            if UNSIGNED.contains(&name.as_str()) {
                continue;
            }
            let value = value
                .to_str()
                .map_err(|_| format!("header {name} is not visible ASCII and cannot be signed"))?;
            headers.push((name.as_str().to_string(), value.to_string()));
        }
        headers.push(("host".to_string(), host));
        let amz_date = amz_date(now);
        headers.push(("x-amz-date".to_string(), amz_date.clone()));
        if let Some(token) = &self.credentials.session_token {
            headers.push(("x-amz-security-token".to_string(), token.clone()));
        }
        let payload_hash = hex(digest::digest(&digest::SHA256, request.body()).as_ref());
        let signed = self.signature(
            request.method().as_str(),
            // The path as it goes on the wire, already percent-encoded once: encoding it again
            // is the "twice" AWS asks of every service but S3.
            request.uri().path(),
            request.uri().query().unwrap_or(""),
            &headers,
            &payload_hash,
            &amz_date,
        );
        let mut set = |name: &'static str, value: &str| -> Result<(), String> {
            let value = HeaderValue::from_str(value)
                .map_err(|_| format!("the {name} value is not a valid header"))?;
            request.headers_mut().insert(name, value);
            Ok(())
        };
        set("x-amz-date", &signed.amz_date)?;
        if let Some(token) = &self.credentials.session_token {
            set("x-amz-security-token", token)?;
        }
        set("authorization", &signed.authorization)?;
        Ok(())
    }

    /// The signature over the parts of a request. `headers` holds every header to sign,
    /// `host` and `x-amz-date` included, with names in any case.
    pub(crate) fn signature(
        &self,
        method: &str,
        path: &str,
        query: &str,
        headers: &[(String, String)],
        payload_hash: &str,
        amz_date: &str,
    ) -> Signed {
        let (canonical, signed_headers) =
            canonical_request(method, path, query, headers, payload_hash);
        let date = &amz_date[..8];
        let scope = format!(
            "{date}/{}/{}/aws4_request",
            self.scope.region, self.scope.service
        );
        let to_sign = string_to_sign(amz_date, &scope, &canonical);
        let key = signing_key(
            &self.credentials.secret_access_key,
            date,
            &self.scope.region,
            &self.scope.service,
        );
        let signature = hex(hmac::sign(&key, to_sign.as_bytes()).as_ref());
        Signed {
            amz_date: amz_date.to_string(),
            authorization: format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
                self.credentials.access_key_id
            ),
        }
    }
}

/// The canonical request and its `SignedHeaders` list.
pub(crate) fn canonical_request(
    method: &str,
    path: &str,
    query: &str,
    headers: &[(String, String)],
    payload_hash: &str,
) -> (String, String) {
    // Lowercase names, sorted; repeated names joined with commas in the order they came;
    // values trimmed, inner runs of whitespace (a folded line included) collapsed to one space.
    let mut merged: Vec<(String, String)> = Vec::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        match merged.iter_mut().find(|(n, _)| *n == name) {
            Some((_, existing)) => {
                existing.push(',');
                existing.push_str(&value);
            }
            None => merged.push((name, value)),
        }
    }
    merged.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical_headers: String = merged
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();
    let signed_headers = merged
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical = format!(
        "{method}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        canonical_path(path),
        canonical_query(query),
    );
    (canonical, signed_headers)
}

/// The path with `.` and `..` segments resolved and repeated slashes collapsed, each byte
/// outside the unreserved set percent-encoded (`/` kept).
fn canonical_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    let mut out = String::from("/");
    out.push_str(
        &segments
            .iter()
            .map(|s| uri_encode(s.as_bytes()))
            .collect::<Vec<_>>()
            .join("/"),
    );
    if segments.is_empty() {
        return out;
    }
    if path.ends_with('/') {
        out.push('/');
    }
    out
}

/// Parameters decoded and re-encoded with AWS's rules, sorted by name then value.
fn canonical_query(query: &str) -> String {
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                uri_encode(&percent_decode(name)),
                uri_encode(&percent_decode(value)),
            )
        })
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn string_to_sign(amz_date: &str, scope: &str, canonical: &str) -> String {
    let hash = hex(digest::digest(&digest::SHA256, canonical.as_bytes()).as_ref());
    format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{hash}")
}

fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> hmac::Key {
    let step = |key: &[u8], data: &str| {
        hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data.as_bytes())
    };
    let k_date = step(format!("AWS4{secret}").as_bytes(), date);
    let k_region = step(k_date.as_ref(), region);
    let k_service = step(k_region.as_ref(), service);
    let k_signing = step(k_service.as_ref(), "aws4_request");
    hmac::Key::new(hmac::HMAC_SHA256, k_signing.as_ref())
}

/// `Host` as reqwest sends it: the host, plus the port unless it is the scheme's default.
fn host_header(uri: &http::Uri) -> Option<String> {
    let host = uri.host()?;
    let default_port = match uri.scheme_str() {
        Some("https") => Some(443),
        Some("http") => Some(80),
        _ => None,
    };
    Some(match uri.port_u16() {
        Some(port) if Some(port) != default_port => format!("{host}:{port}"),
        _ => host.to_string(),
    })
}

/// `20150830T123600Z`.
pub(crate) fn amz_date(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Year, month, day of a count of days since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Percent-encode every byte but `A-Z a-z 0-9 - _ . ~`, with uppercase hex.
fn uri_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match decoded {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The exporters' blocking HTTP client, signing each request before sending it.
///
/// A request that already carries `Authorization` (an API key set through
/// `OTEL_EXPORTER_OTLP_HEADERS`) goes out as it is.
#[derive(Debug)]
pub(crate) struct SigningClient {
    pub(crate) inner: reqwest::blocking::Client,
    pub(crate) signer: Signer,
}

#[async_trait::async_trait]
impl HttpClient for SigningClient {
    async fn send_bytes(&self, mut request: Request<Bytes>) -> Result<Response<Bytes>, HttpError> {
        if !request.headers().contains_key(http::header::AUTHORIZATION) {
            self.signer.sign(&mut request, SystemTime::now())?;
        }
        self.inner.send_bytes(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One case of the official test suite (`tests/fixtures/sigv4`).
    struct Case {
        name: String,
        method: String,
        path: String,
        query: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        context: serde_json::Value,
    }

    fn case(dir: &std::path::Path) -> Case {
        let request = std::fs::read_to_string(dir.join("request.txt")).unwrap();
        let (head, body) = request.split_once("\n\n").unwrap_or((&request, ""));
        let mut lines = head.lines();
        let line = lines.next().unwrap();
        let (method, target) = line.split_once(' ').unwrap();
        let target = target.strip_suffix(" HTTP/1.1").unwrap();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let mut headers: Vec<(String, String)> = Vec::new();
        for line in lines {
            if line.starts_with(char::is_whitespace) {
                // A folded line continues the previous value.
                let last = headers.last_mut().unwrap();
                last.1.push(' ');
                last.1.push_str(line.trim());
            } else {
                let (name, value) = line.split_once(':').unwrap();
                headers.push((name.to_string(), value.to_string()));
            }
        }
        let context = std::fs::read_to_string(dir.join("context.json")).unwrap();
        Case {
            name: dir.file_name().unwrap().to_string_lossy().into_owned(),
            method: method.to_string(),
            path: path.to_string(),
            query: query.to_string(),
            headers,
            body: body.as_bytes().to_vec(),
            context: serde_json::from_str(&context).unwrap(),
        }
    }

    #[test]
    fn the_official_test_suite() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sigv4");
        if !root.exists() {
            // A packaged crate leaves `tests/` out; the repository always has it.
            eprintln!("skipped: {} is not here", root.display());
            return;
        }
        let mut checked = Vec::new();
        for entry in std::fs::read_dir(&root).unwrap() {
            let dir = entry.unwrap().path();
            if !dir.is_dir() {
                continue;
            }
            let case = case(&dir);
            let ctx = &case.context;
            // Out of scope: S3's unnormalized paths, and a token added after signing.
            if ctx["normalize"] == false || ctx["omit_session_token"] == true {
                continue;
            }
            let credentials = &ctx["credentials"];
            let signer = Signer {
                credentials: Credentials {
                    access_key_id: credentials["access_key_id"].as_str().unwrap().into(),
                    secret_access_key: credentials["secret_access_key"].as_str().unwrap().into(),
                    session_token: credentials["token"].as_str().map(Into::into),
                },
                scope: Scope {
                    service: ctx["service"].as_str().unwrap().into(),
                    region: ctx["region"].as_str().unwrap().into(),
                },
            };
            let amz_date = "20150830T123600Z";
            let payload_hash = hex(digest::digest(&digest::SHA256, &case.body).as_ref());
            let mut headers = case.headers.clone();
            headers.push(("x-amz-date".into(), amz_date.into()));
            if ctx["sign_body"] == true {
                headers.push(("x-amz-content-sha256".into(), payload_hash.clone()));
            }
            if let Some(token) = &signer.credentials.session_token {
                headers.push(("x-amz-security-token".into(), token.clone()));
            }
            let expected = |file: &str| std::fs::read_to_string(dir.join(file)).unwrap();
            let (canonical, _) = canonical_request(
                &case.method,
                &case.path,
                &case.query,
                &headers,
                &payload_hash,
            );
            assert_eq!(
                canonical,
                expected("header-canonical-request.txt"),
                "{}",
                case.name
            );
            let scope = format!(
                "20150830/{}/{}/aws4_request",
                signer.scope.region, signer.scope.service
            );
            assert_eq!(
                string_to_sign(amz_date, &scope, &canonical),
                expected("header-string-to-sign.txt"),
                "{}",
                case.name
            );
            let signed = signer.signature(
                &case.method,
                &case.path,
                &case.query,
                &headers,
                &payload_hash,
                amz_date,
            );
            let signature = expected("header-signature.txt");
            assert!(
                signed
                    .authorization
                    .ends_with(&format!("Signature={}", signature.trim())),
                "{}: {}",
                case.name,
                signed.authorization
            );
            checked.push(case.name);
        }
        assert!(
            checked.len() >= 25,
            "only {} cases ran: {checked:?}",
            checked.len()
        );
        eprintln!("{} cases of the AWS test suite", checked.len());
    }

    #[test]
    fn scopes_from_aws_hosts() {
        let scope = |service: &str, region: &str| {
            Some(Scope {
                service: service.into(),
                region: region.into(),
            })
        };
        assert_eq!(
            Scope::of_host("xray.eu-west-1.amazonaws.com"),
            scope("xray", "eu-west-1")
        );
        assert_eq!(
            Scope::of_host("XRAY-FIPS.us-east-1.amazonaws.com"),
            scope("xray", "us-east-1")
        );
        assert_eq!(
            Scope::of_host("logs.cn-north-1.amazonaws.com.cn"),
            scope("logs", "cn-north-1")
        );
        assert_eq!(
            Scope::of_host("monitoring.ap-southeast-2.amazonaws.com"),
            scope("monitoring", "ap-southeast-2")
        );
        for other in [
            "localhost",
            "collector.internal",
            "s3.eu-west-1.amazonaws.com",
            "xray.amazonaws.com",
            "xray.eu-west-1.amazonaws.com.evil.example",
            "xray.eu_west.amazonaws.com",
        ] {
            assert_eq!(Scope::of_host(other), None, "{other}");
        }
    }

    #[test]
    fn dates() {
        assert_eq!(amz_date(UNIX_EPOCH), "19700101T000000Z");
        // 2015-08-30T12:36:00Z, the test suite's time.
        let suite = UNIX_EPOCH + Duration::from_secs(1_440_938_160);
        assert_eq!(amz_date(suite), "20150830T123600Z");
        // A leap day and the last second of a year.
        assert_eq!(
            amz_date(UNIX_EPOCH + Duration::from_secs(1_709_208_000)),
            "20240229T120000Z"
        );
        assert_eq!(
            amz_date(UNIX_EPOCH + Duration::from_secs(1_767_225_599)),
            "20251231T235959Z"
        );
    }

    #[test]
    fn secrets_stay_out_of_debug() {
        let credentials = Credentials::read(|name| match name {
            "AWS_ACCESS_KEY_ID" => Some("AKIDEXAMPLE".into()),
            "AWS_SECRET_ACCESS_KEY" => Some("very-secret".into()),
            "AWS_SESSION_TOKEN" => Some("token-secret".into()),
            _ => None,
        })
        .unwrap();
        let printed = format!("{credentials:?}");
        assert!(printed.contains("AKIDEXAMPLE"));
        assert!(!printed.contains("very-secret"), "{printed}");
        assert!(!printed.contains("token-secret"), "{printed}");
    }

    #[test]
    fn credentials_need_both_key_and_secret() {
        assert!(
            Credentials::read(|name| (name == "AWS_ACCESS_KEY_ID").then(|| "AKID".into()))
                .is_none()
        );
    }

    #[test]
    fn a_request_is_signed_with_host_date_and_token() {
        let signer = Signer {
            credentials: Credentials {
                access_key_id: "AKIDEXAMPLE".into(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
                session_token: Some("token".into()),
            },
            scope: Scope {
                service: "xray".into(),
                region: "eu-west-1".into(),
            },
        };
        let mut request = Request::post("https://xray.eu-west-1.amazonaws.com:443/v1/traces")
            .header("content-type", "application/x-protobuf")
            .header("user-agent", "OTel-OTLP-Exporter-Rust/0.33.1")
            .body(Bytes::from_static(b"spans"))
            .unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_440_938_160);
        signer.sign(&mut request, now).unwrap();
        let headers = request.headers();
        assert_eq!(headers["x-amz-date"], "20150830T123600Z");
        assert_eq!(headers["x-amz-security-token"], "token");
        let authorization = headers["authorization"].to_str().unwrap();
        assert!(
            authorization.starts_with(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/eu-west-1/xray/aws4_request, \
                 SignedHeaders=content-type;host;x-amz-date;x-amz-security-token, Signature="
            ),
            "{authorization}"
        );
        // The default port is not part of the signed `host`, as it is not part of what reqwest
        // sends.
        let (canonical, _) = canonical_request(
            "POST",
            "/v1/traces",
            "",
            &[("host".into(), host_header(request.uri()).unwrap())],
            "",
        );
        assert!(canonical.contains("host:xray.eu-west-1.amazonaws.com\n"));
    }
}
