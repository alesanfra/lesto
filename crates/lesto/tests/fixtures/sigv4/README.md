The AWS Signature Version 4 test suite, as published in
[awslabs/aws-c-auth](https://github.com/awslabs/aws-c-auth/tree/main/tests/aws-signing-test-suite/v4)
(Apache-2.0, Copyright Amazon.com, Inc. or its affiliates). Only the header-signing files are
kept. `src/sigv4.rs` checks its canonical request, string to sign and signature against every
case it applies to (normalized paths, session token signed).
