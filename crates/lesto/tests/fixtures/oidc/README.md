Test-only signing keys for `tests/oidc.rs` and `examples/99-tutorial/src/oidc.rs`. They sign
tokens for a provider the tests run on `127.0.0.1:0`; they protect nothing and must never be used
anywhere else. Generated with:

```sh
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out rsa-1.pem
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out rsa-2.pem
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out ec.pem
```
