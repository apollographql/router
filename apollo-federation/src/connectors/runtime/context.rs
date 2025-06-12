pub trait ContextReader {
    fn get_key(&self, key: &str) -> Option<serde_json_bytes::Value>;
}
