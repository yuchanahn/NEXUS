use llm_adapters::{AdapterConfig, HttpProvider};
use serde_json::json;
fn config(url: &str, allow: bool) -> AdapterConfig {
    serde_json::from_value(json!({"id":"docker","protocol":"chat","base_url":url,"allow_http":allow,"models":[{"id":"model","upstream":"model","status":"test"}]})).unwrap()
}
#[test]
fn private_docker_http_requires_owner_opt_in_and_never_allows_url_credentials() {
    assert!(HttpProvider::new(config("http://fixture:8090/v1", false)).is_err());
    assert!(HttpProvider::new(config("http://fixture:8090/v1", true)).is_ok());
    assert!(HttpProvider::new(config("http://secret@fixture:8090/v1", true)).is_err());
    assert!(HttpProvider::new(config("http://fixture:8090/v1?token=secret", true)).is_err());
    assert!(HttpProvider::new(config("https://remote.example/v1", false)).is_ok());
}
