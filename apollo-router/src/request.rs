use bytes::Bytes;
use derivative::Derivative;
use serde::de::Error;
use serde::Deserialize;
use serde::Serialize;

use crate::json_ext::Object;
use crate::json_ext::Value;

/// A graphql request.
/// Used for federated and subgraph queries.
#[derive(Clone, Derivative, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[derivative(Debug, PartialEq, Eq, Hash)]
pub struct Request {
    /// The graphql query.
    pub query: Option<String>,

    /// The optional graphql operation.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub operation_name: Option<String>,

    /// The optional variables in the form of a json object.
    #[serde(
        skip_serializing_if = "Object::is_empty",
        default,
        deserialize_with = "deserialize_null_default"
    )]
    pub variables: Object,

    ///  extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    pub extensions: Object,
}

// NOTE: this deserialize helper is used to transform `null` to Default::default()
fn deserialize_null_default<'de, D, T: Default + Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
{
    <Option<T>>::deserialize(deserializer).map(|x| x.unwrap_or_default())
}

#[buildstructor::buildstructor]
impl Request {
    #[builder]
    pub fn new(
        query: Option<String>,
        operation_name: Option<String>,
        variables: Option<Object>,
        extensions: Option<Object>,
    ) -> Self {
        Self {
            query,
            operation_name,
            variables: variables.unwrap_or_default(),
            extensions: extensions.unwrap_or_default(),
        }
    }

    #[builder]
    pub fn fake_new(
        query: Option<String>,
        operation_name: Option<String>,
        variables: Option<Object>,
        extensions: Option<Object>,
    ) -> Self {
        Self {
            query,
            operation_name,
            variables: variables.unwrap_or_default(),
            extensions: extensions.unwrap_or_default(),
        }
    }

    pub fn from_urlencoded_query(url_encoded_query: String) -> Result<Request, serde_json::Error> {
        // As explained in the form content types specification https://www.w3.org/TR/html4/interact/forms.html#h-17.13.4.1
        // `Forms submitted with this content type must be encoded as follows:`
        //
        // Space characters are replaced by `+', and then reserved characters are escaped as described in [RFC1738], section 2.2
        // The real percent encoding uses `%20` while form data in URLs uses `+`.
        // This can be seen empirically by running a CURL request with a --data-urlencoded that contains spaces.
        // however, quoting the urlencoding docs https://docs.rs/urlencoding/latest/urlencoding/fn.decode_binary.html
        // `Unencoded `+` is preserved literally, and _not_ changed to a space.`
        //
        // We will thus replace '+' by "%20" below so we comply with the percent encoding specification, before decoding the parameters.
        let query = url_encoded_query.replace('+', "%20");
        let decoded_string = urlencoding::decode_binary(query.as_bytes());
        let urldecoded: serde_json::Value =
            serde_urlencoded::from_bytes(&decoded_string).map_err(serde_json::Error::custom)?;

        let operation_name = if let Some(serde_json::Value::String(operation_name)) =
            urldecoded.get("operationName")
        {
            Some(operation_name.clone())
        } else {
            None
        };

        let query = if let Some(serde_json::Value::String(query)) = urldecoded.get("query") {
            Some(query.as_str())
        } else {
            None
        };
        let variables: Object = get_from_urldecoded(&urldecoded, "variables")?.unwrap_or_default();
        let extensions: Object =
            get_from_urldecoded(&urldecoded, "extensions")?.unwrap_or_default();

        let request_builder = Self::builder()
            .variables(variables)
            .and_operation_name(operation_name)
            .extensions(extensions);

        let request = if let Some(query_str) = query {
            request_builder.query(query_str).build()
        } else {
            request_builder.build()
        };

        Ok(request)
    }

    pub fn from_bytes(b: Bytes) -> Result<Request, serde_json::error::Error> {
        let value = Value::from_bytes(b)?;
        let mut object = ensure_object!(value).map_err(serde::de::Error::custom)?;

        let variables = extract_key_value_from_object!(object, "variables", Value::Object(o) => o)
            .map_err(serde::de::Error::custom)?
            .unwrap_or_default();
        let extensions =
            extract_key_value_from_object!(object, "extensions", Value::Object(o) => o)
                .map_err(serde::de::Error::custom)?
                .unwrap_or_default();
        let query = extract_key_value_from_object!(object, "query", Value::String(s) => s)
            .map_err(serde::de::Error::custom)?
            .map(|s| s.as_str().to_string());
        let operation_name =
            extract_key_value_from_object!(object, "operation_name", Value::String(s) => s)
                .map_err(serde::de::Error::custom)?
                .map(|s| s.as_str().to_string());

        Ok(Request {
            query,
            operation_name,
            variables,
            extensions,
        })
    }
}

fn get_from_urldecoded<'a, T: Deserialize<'a>>(
    object: &'a serde_json::Value,
    key: &str,
) -> Result<Option<T>, serde_json::Error> {
    if let Some(serde_json::Value::String(byte_string)) = object.get(key) {
        Some(serde_json::from_str(byte_string.as_str())).transpose()
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use serde_json_bytes::json as bjson;
    use test_log::test;

    use super::*;

    #[test]
    fn test_request() {
        let data = json!(
        {
          "query": "query aTest($arg1: String!) { test(who: $arg1) }",
          "operationName": "aTest",
          "variables": { "arg1": "me" },
          "extensions": {"extension": 1}
        })
        .to_string();
        println!("data: {}", data);
        let result = serde_json::from_str::<Request>(data.as_str());
        println!("result: {:?}", result);
        assert_eq!(
            result.unwrap(),
            Request::builder()
                .query("query aTest($arg1: String!) { test(who: $arg1) }".to_owned())
                .operation_name("aTest")
                .variables(bjson!({ "arg1": "me" }).as_object().unwrap().clone())
                .extensions(bjson!({"extension": 1}).as_object().cloned().unwrap())
                .build()
        );
    }

    #[test]
    fn test_no_variables() {
        let result = serde_json::from_str::<Request>(
            json!(
            {
              "query": "query aTest($arg1: String!) { test(who: $arg1) }",
              "operationName": "aTest",
              "extensions": {"extension": 1}
            })
            .to_string()
            .as_str(),
        );
        assert_eq!(
            result.unwrap(),
            Request::builder()
                .query("query aTest($arg1: String!) { test(who: $arg1) }".to_owned())
                .operation_name("aTest")
                .extensions(bjson!({"extension": 1}).as_object().cloned().unwrap())
                .build()
        );
    }

    #[test]
    // rover sends { "variables": null } when running the introspection query,
    // and possibly running other queries as well.
    fn test_variables_is_null() {
        let result = serde_json::from_str::<Request>(
            json!(
            {
              "query": "query aTest($arg1: String!) { test(who: $arg1) }",
              "operationName": "aTest",
              "variables": null,
              "extensions": {"extension": 1}
            })
            .to_string()
            .as_str(),
        );
        assert_eq!(
            result.unwrap(),
            Request::builder()
                .query("query aTest($arg1: String!) { test(who: $arg1) }")
                .operation_name("aTest")
                .extensions(bjson!({"extension": 1}).as_object().cloned().unwrap())
                .build()
        );
    }

    #[test]
    fn from_urlencoded_query_works() {
        let query_string = "query=%7B+topProducts+%7B+upc+name+reviews+%7B+id+product+%7B+name+%7D+author+%7B+id+name+%7D+%7D+%7D+%7D&extensions=%7B+%22persistedQuery%22+%3A+%7B+%22version%22+%3A+1%2C+%22sha256Hash%22+%3A+%2220a101de18d4a9331bfc4ccdfef33cc735876a689490433570f17bdd4c0bad3f%22+%7D+%7D".to_string();

        let expected_result = serde_json::from_str::<Request>(
            json!(
            {
              "query": "{ topProducts { upc name reviews { id product { name } author { id name } } } }",
              "extensions": {
                  "persistedQuery": {
                      "version": 1,
                      "sha256Hash": "20a101de18d4a9331bfc4ccdfef33cc735876a689490433570f17bdd4c0bad3f"
                  }
                }
            })
            .to_string()
            .as_str(),
        ).unwrap();

        let req = Request::from_urlencoded_query(query_string).unwrap();

        assert_eq!(expected_result, req);
    }
}
