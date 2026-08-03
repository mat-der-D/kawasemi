use super::*;

// --- test fixtures -------------------------------------------------------

const KEY_ID: &str = "https://kawasemi.example/actors/alice#main-key";
const DATE: &str = "Tue, 07 Jun 2022 20:51:21 GMT";
const HOST: &str = "remote.example";
const DIGEST: &str = "SHA-256=X48E9qOokqqrvdts8nOJRJN3OWDUoyWxBf7kbu9DBPE=";

fn signable_get() -> SignableRequest {
    let mut req = SignableRequest::new(Method::POST, "https://remote.example/inbox?x=1", KEY_ID);
    req.headers.insert("host", HOST.parse().unwrap());
    req.headers.insert("date", DATE.parse().unwrap());
    req
}

fn signable_post_with_body() -> SignableRequest {
    let mut req = signable_get();
    req.headers.insert("digest", DIGEST.parse().unwrap());
    req
}

// =========================================================================
// draft-cavage
// =========================================================================

mod draft_cavage {
    use super::*;

    #[test]
    fn build_signing_input_covers_request_target_host_and_date_without_a_body() {
        let suite = DraftCavageSuite::new();
        let req = signable_get();

        let input = suite.build_signing_input(&req);

        assert_eq!(input.format, SignatureFormat::DraftCavage);
        assert_eq!(
            input.covered_components,
            vec!["(request-target)", "host", "date"]
        );
        assert_eq!(
            input.signing_string,
            format!("(request-target): post /inbox?x=1\nhost: {HOST}\ndate: {DATE}")
        );
    }

    #[test]
    fn build_signing_input_includes_digest_only_when_present() {
        let suite = DraftCavageSuite::new();
        let req = signable_post_with_body();

        let input = suite.build_signing_input(&req);

        assert_eq!(
            input.covered_components,
            vec!["(request-target)", "host", "date", "digest"]
        );
        assert!(input.signing_string.ends_with(&format!("digest: {DIGEST}")));
    }

    #[test]
    fn request_target_lowercases_the_method_but_not_the_path() {
        let suite = DraftCavageSuite::new();
        let mut req = SignableRequest::new(Method::GET, "https://remote.example/Inbox", KEY_ID);
        req.headers.insert("host", HOST.parse().unwrap());
        req.headers.insert("date", DATE.parse().unwrap());

        let input = suite.build_signing_input(&req);

        assert!(
            input
                .signing_string
                .starts_with("(request-target): get /Inbox")
        );
    }

    #[test]
    fn assemble_headers_produces_a_single_signature_header_with_expected_params() {
        let suite = DraftCavageSuite::new();
        let req = signable_post_with_body();
        let input = suite.build_signing_input(&req);
        let signature_bytes = b"fake-signature-bytes";

        let headers = suite.assemble_headers(KEY_ID, signature_bytes, &input);

        assert_eq!(headers.len(), 1);
        let (name, value) = &headers[0];
        assert_eq!(name, "Signature");
        assert!(value.contains(&format!("keyId=\"{KEY_ID}\"")));
        assert!(value.contains("algorithm=\"rsa-sha256\""));
        assert!(value.contains("headers=\"(request-target) host date digest\""));
        assert!(value.contains(&format!(
            "signature=\"{}\"",
            BASE64_STANDARD.encode(signature_bytes)
        )));
    }

    #[test]
    fn parse_round_trips_assemble_headers_output() {
        let suite = DraftCavageSuite::new();
        let req = signable_post_with_body();
        let input = suite.build_signing_input(&req);
        let signature_bytes = b"another-fake-signature".to_vec();
        let assembled = suite.assemble_headers(KEY_ID, &signature_bytes, &input);

        let mut received = RequestHeaders::new();
        for (name, value) in &assembled {
            received.insert(
                axum::http::HeaderName::try_from(name.as_str()).unwrap(),
                value.parse().unwrap(),
            );
        }

        let parsed = suite
            .parse(&received)
            .expect("assembled headers must parse");

        assert_eq!(parsed.format, SignatureFormat::DraftCavage);
        assert_eq!(parsed.key_id, KEY_ID);
        assert_eq!(
            parsed.covered_components,
            vec!["(request-target)", "host", "date", "digest"]
        );
        assert_eq!(parsed.signature, signature_bytes);
        assert_eq!(parsed.algorithm.as_deref(), Some("rsa-sha256"));
    }

    #[test]
    fn parse_accepts_the_legacy_authorization_signature_form() {
        let suite = DraftCavageSuite::new();
        let mut headers = RequestHeaders::new();
        headers.insert(
            "authorization",
            "Signature keyId=\"k1\",algorithm=\"rsa-sha256\",headers=\"(request-target) host date\",signature=\"c2ln\""
                .parse()
                .unwrap(),
        );

        let parsed = suite
            .parse(&headers)
            .expect("legacy Authorization form must parse");

        assert_eq!(parsed.key_id, "k1");
        assert_eq!(
            parsed.covered_components,
            vec!["(request-target)", "host", "date"]
        );
    }

    #[test]
    fn parse_fails_when_signature_header_is_missing() {
        let suite = DraftCavageSuite::new();
        let headers = RequestHeaders::new();

        let result = suite.parse(&headers);

        assert!(result.is_err());
    }

    #[test]
    fn parse_fails_when_headers_param_is_missing() {
        let suite = DraftCavageSuite::new();
        let mut headers = RequestHeaders::new();
        headers.insert(
            "signature",
            "keyId=\"k1\",algorithm=\"rsa-sha256\",signature=\"c2ln\""
                .parse()
                .unwrap(),
        );

        let result = suite.parse(&headers);

        assert!(result.is_err());
    }
}

// =========================================================================
// RFC 9421
// =========================================================================

mod rfc9421 {
    use super::*;

    #[test]
    fn build_signing_input_covers_method_target_uri_host_and_date_without_a_body() {
        let suite = Rfc9421Suite::new();
        let req = signable_get();

        let input = suite.build_signing_input(&req);

        assert_eq!(input.format, SignatureFormat::Rfc9421);
        assert_eq!(
            input.covered_components,
            vec!["@method", "@target-uri", "host", "date"]
        );
        assert_eq!(input.signed_key_id.as_deref(), Some(KEY_ID));
        let expected = format!(
            "\"@method\": POST\n\"@target-uri\": https://remote.example/inbox?x=1\n\"host\": {HOST}\n\"date\": {DATE}\n\"@signature-params\": (\"@method\" \"@target-uri\" \"host\" \"date\");keyid=\"{KEY_ID}\";alg=\"rsa-v1_5-sha256\""
        );
        assert_eq!(input.signing_string, expected);
    }

    #[test]
    fn build_signing_input_includes_digest_component_only_when_present() {
        let suite = Rfc9421Suite::new();
        let req = signable_post_with_body();

        let input = suite.build_signing_input(&req);

        assert_eq!(
            input.covered_components,
            vec!["@method", "@target-uri", "host", "date", "digest"]
        );
        assert!(
            input
                .signing_string
                .contains(&format!("\"digest\": {DIGEST}"))
        );
        assert!(
            input
                .signing_string
                .contains("(\"@method\" \"@target-uri\" \"host\" \"date\" \"digest\")")
        );
    }

    #[test]
    fn assemble_headers_produces_signature_input_and_signature_headers() {
        let suite = Rfc9421Suite::new();
        let req = signable_post_with_body();
        let input = suite.build_signing_input(&req);
        let signature_bytes = b"fake-rfc9421-signature";

        let headers = suite.assemble_headers(KEY_ID, signature_bytes, &input);

        assert_eq!(headers.len(), 2);
        let signature_input = headers
            .iter()
            .find(|(name, _)| name == "Signature-Input")
            .expect("Signature-Input header must be present");
        let signature = headers
            .iter()
            .find(|(name, _)| name == "Signature")
            .expect("Signature header must be present");

        assert_eq!(
            signature_input.1,
            format!(
                "sig1=(\"@method\" \"@target-uri\" \"host\" \"date\" \"digest\");keyid=\"{KEY_ID}\";alg=\"rsa-v1_5-sha256\""
            )
        );
        assert_eq!(
            signature.1,
            format!("sig1=:{}:", BASE64_STANDARD.encode(signature_bytes))
        );
    }

    #[test]
    fn parse_round_trips_assemble_headers_output() {
        let suite = Rfc9421Suite::new();
        let req = signable_post_with_body();
        let input = suite.build_signing_input(&req);
        let signature_bytes = b"another-fake-rfc9421-signature".to_vec();
        let assembled = suite.assemble_headers(KEY_ID, &signature_bytes, &input);

        let mut received = RequestHeaders::new();
        for (name, value) in &assembled {
            received.insert(
                axum::http::HeaderName::try_from(name.as_str()).unwrap(),
                value.parse().unwrap(),
            );
        }

        let parsed = suite
            .parse(&received)
            .expect("assembled headers must parse");

        assert_eq!(parsed.format, SignatureFormat::Rfc9421);
        assert_eq!(parsed.key_id, KEY_ID);
        assert_eq!(
            parsed.covered_components,
            vec!["@method", "@target-uri", "host", "date", "digest"]
        );
        assert_eq!(parsed.signature, signature_bytes);
        assert_eq!(parsed.algorithm.as_deref(), Some("rsa-v1_5-sha256"));
    }

    #[test]
    fn parse_fails_when_signature_input_header_is_missing() {
        let suite = Rfc9421Suite::new();
        let mut headers = RequestHeaders::new();
        headers.insert("signature", "sig1=:c2ln:".parse().unwrap());

        let result = suite.parse(&headers);

        assert!(result.is_err());
    }

    #[test]
    fn parse_fails_when_signature_header_is_missing() {
        let suite = Rfc9421Suite::new();
        let mut headers = RequestHeaders::new();
        headers.insert(
            "signature-input",
            "sig1=(\"@method\" \"host\");keyid=\"k1\";alg=\"rsa-v1_5-sha256\""
                .parse()
                .unwrap(),
        );

        let result = suite.parse(&headers);

        assert!(result.is_err());
    }

    #[test]
    fn parse_fails_when_keyid_param_is_missing() {
        let suite = Rfc9421Suite::new();
        let mut headers = RequestHeaders::new();
        headers.insert(
            "signature-input",
            "sig1=(\"@method\" \"host\");alg=\"rsa-v1_5-sha256\""
                .parse()
                .unwrap(),
        );
        headers.insert("signature", "sig1=:c2ln:".parse().unwrap());

        let result = suite.parse(&headers);

        assert!(result.is_err());
    }
}

// =========================================================================
// format detection (the task's explicit observable completion condition:
// "受信ヘッダから形式を検出できる")
// =========================================================================

mod detect {
    use super::*;

    #[test]
    fn detects_rfc9421_when_signature_input_header_is_present() {
        let mut headers = RequestHeaders::new();
        headers.insert(
            "signature-input",
            "sig1=(\"@method\");keyid=\"k1\";alg=\"rsa-v1_5-sha256\""
                .parse()
                .unwrap(),
        );
        headers.insert("signature", "sig1=:c2ln:".parse().unwrap());

        assert_eq!(
            Rfc9421Suite::detect(&headers),
            Some(SignatureFormat::Rfc9421)
        );
        assert_eq!(
            DraftCavageSuite::detect(&headers),
            Some(SignatureFormat::Rfc9421)
        );
    }

    #[test]
    fn detects_draft_cavage_when_only_signature_header_is_present() {
        let mut headers = RequestHeaders::new();
        headers.insert(
            "signature",
            "keyId=\"k1\",algorithm=\"rsa-sha256\",headers=\"(request-target) host date\",signature=\"c2ln\""
                .parse()
                .unwrap(),
        );

        assert_eq!(
            DraftCavageSuite::detect(&headers),
            Some(SignatureFormat::DraftCavage)
        );
    }

    #[test]
    fn detects_draft_cavage_from_legacy_authorization_header() {
        let mut headers = RequestHeaders::new();
        headers.insert(
            "authorization",
            "Signature keyId=\"k1\",algorithm=\"rsa-sha256\",headers=\"date\",signature=\"c2ln\""
                .parse()
                .unwrap(),
        );

        assert_eq!(
            DraftCavageSuite::detect(&headers),
            Some(SignatureFormat::DraftCavage)
        );
    }

    #[test]
    fn detects_nothing_when_neither_signature_header_is_present() {
        let headers = RequestHeaders::new();

        assert_eq!(DraftCavageSuite::detect(&headers), None);
    }

    #[test]
    fn detects_nothing_for_an_unrelated_authorization_scheme() {
        let mut headers = RequestHeaders::new();
        headers.insert("authorization", "Bearer some-token".parse().unwrap());

        assert_eq!(DraftCavageSuite::detect(&headers), None);
    }

    #[test]
    fn full_round_trip_build_assemble_detect_parse_for_each_format() {
        for suite_format in [SignatureFormat::DraftCavage, SignatureFormat::Rfc9421] {
            let req = signable_post_with_body();
            let signature_bytes = b"round-trip-signature".to_vec();

            let assembled: Vec<(String, String)> = match suite_format {
                SignatureFormat::DraftCavage => {
                    let suite = DraftCavageSuite::new();
                    let input = suite.build_signing_input(&req);
                    suite.assemble_headers(KEY_ID, &signature_bytes, &input)
                }
                SignatureFormat::Rfc9421 => {
                    let suite = Rfc9421Suite::new();
                    let input = suite.build_signing_input(&req);
                    suite.assemble_headers(KEY_ID, &signature_bytes, &input)
                }
            };

            let mut received = RequestHeaders::new();
            for (name, value) in &assembled {
                received.insert(
                    axum::http::HeaderName::try_from(name.as_str()).unwrap(),
                    value.parse().unwrap(),
                );
            }

            let detected = DraftCavageSuite::detect(&received).expect("format must be detected");
            assert_eq!(detected, suite_format);

            let parsed = match detected {
                SignatureFormat::DraftCavage => DraftCavageSuite::new().parse(&received),
                SignatureFormat::Rfc9421 => Rfc9421Suite::new().parse(&received),
            }
            .expect("assembled headers for the detected format must parse");

            assert_eq!(parsed.key_id, KEY_ID);
            assert_eq!(parsed.signature, signature_bytes);
        }
    }
}
