use serde_json_bytes::Value;

pub(super) fn encode_json_as_form(value: &Value) -> Result<String, &'static str> {
    if value.as_object().is_none() {
        return Err("Expected URL-encoded forms to be objects");
    }

    let mut encoded: form_urlencoded::Serializer<String> =
        form_urlencoded::Serializer::new(String::new());

    fn encode(encoded: &mut form_urlencoded::Serializer<String>, value: &Value, prefix: &str) {
        match value {
            Value::Null => {
                encoded.append_pair(prefix, "");
            }
            Value::String(s) => {
                encoded.append_pair(prefix, s.as_str());
            }
            Value::Bool(b) => {
                encoded.append_pair(prefix, if *b { "true" } else { "false" });
            }
            Value::Number(n) => {
                encoded.append_pair(prefix, &n.to_string());
            }
            Value::Array(array) => {
                for (i, value) in array.iter().enumerate() {
                    let prefix = format!("{prefix}[{i}]");
                    encode(encoded, value, &prefix);
                }
            }
            Value::Object(obj) => {
                for (key, value) in obj {
                    if prefix.is_empty() {
                        encode(encoded, value, key.as_str())
                    } else {
                        let prefix = format!("{prefix}[{key}]", key = key.as_str());
                        encode(encoded, value, &prefix);
                    };
                }
            }
        }
    }

    encode(&mut encoded, value, "");

    Ok(encoded.finish())
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;

    #[test]
    fn complex() {
        let data = json!({
            "a": 1,
            "b": "2",
            "c": {
                "d": 3,
                "e": "4",
                "f": {
                    "g": 5,
                    "h": "6",
                    "i": [7, 8, 9],
                    "j": [
                        {"k": 10},
                        {"l": 11},
                        {"m": 12}
                    ]
                }
            }
        });

        let encoded = encode_json_as_form(&data).expect("test case is valid for transformation");
        assert_eq!(encoded, "a=1&b=2&c%5Bd%5D=3&c%5Be%5D=4&c%5Bf%5D%5Bg%5D=5&c%5Bf%5D%5Bh%5D=6&c%5Bf%5D%5Bi%5D%5B0%5D=7&c%5Bf%5D%5Bi%5D%5B1%5D=8&c%5Bf%5D%5Bi%5D%5B2%5D=9&c%5Bf%5D%5Bj%5D%5B0%5D%5Bk%5D=10&c%5Bf%5D%5Bj%5D%5B1%5D%5Bl%5D=11&c%5Bf%5D%5Bj%5D%5B2%5D%5Bm%5D=12");
    }

    // https://github.com/ljharb/qs/blob/main/test/stringify.js used as reference for these tests
    #[rstest::rstest]
    #[case(r#"{ "a": "b" }"#, "a=b")]
    #[case(r#"{ "a": 1 }"#, "a=1")]
    #[case(r#"{ "a": 1, "b": 2 }"#, "a=1&b=2")]
    #[case(r#"{ "a": "A_Z" }"#, "a=A_Z")]
    #[case(r#"{ "a": "€" }"#, "a=%E2%82%AC")]
    #[case(r#"{ "a": "" }"#, "a=%EE%80%80")]
    #[case(r#"{ "a": "א" }"#, "a=%D7%90")]
    #[case(r#"{ "a": "𐐷" }"#, "a=%F0%90%90%B7")]
    #[case(r#"{ "a": { "b": "c" } }"#, "a%5Bb%5D=c")]
    #[case(
        r#"{ "a": { "b": { "c": { "d": "e" } } } }"#,
        "a%5Bb%5D%5Bc%5D%5Bd%5D=e"
    )]
    #[case(r#"{ "a": ["b", "c", "d"] }"#, "a%5B0%5D=b&a%5B1%5D=c&a%5B2%5D=d")]
    #[case(r#"{ "a": [], "b": "zz" }"#, "b=zz")]
    #[case(
        r#"{ "a": { "b": ["c", "d"] } }"#,
        "a%5Bb%5D%5B0%5D=c&a%5Bb%5D%5B1%5D=d"
    )]
    #[case(
        r#"{ "a": [",", "", "c,d%"] }"#,
        "a%5B0%5D=%2C&a%5B1%5D=&a%5B2%5D=c%2Cd%25"
    )]
    #[case(r#"{ "a": ",", "b": "", "c": "c,d%" }"#, "a=%2C&b=&c=c%2Cd%25")]
    #[case(r#"{ "a": [{ "b": "c" }] }"#, "a%5B0%5D%5Bb%5D=c")]
    #[case(
        r#"{ "a": [{ "b": { "c": [1] } }] }"#,
        "a%5B0%5D%5Bb%5D%5Bc%5D%5B0%5D=1"
    )]
    #[case(
        r#"{ "a": [{ "b": 1 }, 2, 3] }"#,
        "a%5B0%5D%5Bb%5D=1&a%5B1%5D=2&a%5B2%5D=3"
    )]
    #[case(r#"{ "a": "" }"#, "a=")]
    #[case(r#"{ "a": null }"#, "a=")]
    #[case(r#"{ "a": { "b": "" } }"#, "a%5Bb%5D=")]
    #[case(r#"{ "a": { "b": null } }"#, "a%5Bb%5D=")]
    #[case(r#"{ "a": "b c" }"#, "a=b+c")] // RFC 1738, not RFC 3986 with %20 for spaces!
    #[case(
        r#"{ "my weird field": "~q1!2\"'w$5&7/z8)?" }"#,
        // "my%20weird%20field=~q1%212%22%27w%245%267%2Fz8%29%3F"
        "my+weird+field=%7Eq1%212%22%27w%245%267%2Fz8%29%3F"
    )]
    #[case(r#"{ "a": true }"#, "a=true")]
    #[case(r#"{ "a": { "b": true } }"#, "a%5Bb%5D=true")]
    #[case(r#"{ "b": false }"#, "b=false")]
    #[case(r#"{ "b": { "c": false } }"#, "b%5Bc%5D=false")]
    // #[case(r#"{ "a": [, "2", , , "1"] }"#, "a%5B1%5D=2&a%5B4%5D=1")] // json doesn't do sparse arrays

    fn stringifies_a_querystring_object(#[case] json: &str, #[case] expected: &str) {
        let json = serde_json::from_slice::<Value>(json.as_bytes()).unwrap();
        let encoded = encode_json_as_form(&json).expect("test cases are valid for transformation");
        assert_eq!(encoded, expected);
    }
}
