use super::*;

macro_rules! map {
        ($($tt:tt)*) => {
          serde_json_bytes::json!($($tt)*).as_object().unwrap().clone()
        };
    }

async fn execute(query: &str, variables: JsonMap) -> (serde_json::Value, MockServer) {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/posts")).respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                  "userId": 1,
                  "id": 1,
                  "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
                  "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto"
                },
                {
                  "userId": 1,
                  "id": 2,
                  "title": "qui est esse",
                  "body": "est rerum tempore vitae\nsequi sint nihil reprehenderit dolor beatae ea dolores neque\nfugiat blanditiis voluptate porro vel nihil molestiae ut reiciendis\nqui aperiam non debitis possimus qui neque nisi nulla"
                }]
            )),
        ).mount(&mock_server).await;
    Mock::given(method("GET")).and(path("/posts/1")).respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {
                  "userId": 1,
                  "id": 1,
                  "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
                  "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto"
                }
            )),
        ).mount(&mock_server).await;
    Mock::given(method("GET")).and(path("/posts/2")).respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                  "userId": 1,
                  "id": 2,
                  "title": "qui est esse",
                  "body": "est rerum tempore vitae\nsequi sint nihil reprehenderit dolor beatae ea dolores neque\nfugiat blanditiis voluptate porro vel nihil molestiae ut reiciendis\nqui aperiam non debitis possimus qui neque nisi nulla"
                }
            )),
        ).mount(&mock_server).await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "id": 1,
          "name": "Leanne Graham",
          "username": "Bret",
          "email": "Sincere@april.biz",
          "address": {
            "street": "Kulas Light",
            "suite": "Apt. 556",
            "city": "Gwenborough",
            "zipcode": "92998-3874",
            "geo": {
              "lat": "-37.3159",
              "lng": "81.1496"
            }
          },
          "phone": "1-770-736-8031 x56442",
          "website": "hildegard.org",
          "company": {
            "name": "Romaguera-Crona",
            "catchPhrase": "Multi-layered client-server neural-net",
            "bs": "harness real-time e-markets"
          }
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET")).and(path("/users/1/posts")).respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                  "userId": 1,
                  "id": 1,
                  "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
                  "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto"
                },
                {
                  "userId": 1,
                  "id": 2,
                  "title": "qui est esse",
                  "body": "est rerum tempore vitae\nsequi sint nihil reprehenderit dolor beatae ea dolores neque\nfugiat blanditiis voluptate porro vel nihil molestiae ut reiciendis\nqui aperiam non debitis possimus qui neque nisi nulla"
                }]
            )),
        ).mount(&mock_server).await;

    let res = super::execute(
        &QUICKSTART_SCHEMA.replace("https://jsonplaceholder.typicode.com", &mock_server.uri()),
        &mock_server.uri(),
        query,
        variables,
        None,
        |_| {},
        None,
    )
    .await;

    (res, mock_server)
}

#[tokio::test]
async fn query_1() {
    let query = r#"
          query Posts {
            posts {
              id
              body
              title
            }
          }
        "#;

    let (response, server) = execute(query, Default::default()).await;

    insta::assert_json_snapshot!(response, @r###"
        {
          "data": {
            "posts": [
              {
                "id": 1,
                "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto",
                "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit"
              },
              {
                "id": 2,
                "body": "est rerum tempore vitae\nsequi sint nihil reprehenderit dolor beatae ea dolores neque\nfugiat blanditiis voluptate porro vel nihil molestiae ut reiciendis\nqui aperiam non debitis possimus qui neque nisi nulla",
                "title": "qui est esse"
              }
            ]
          }
        }
        "###);

    req_asserts::matches(
        &server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/posts")],
    );
}

#[tokio::test]
async fn query_2() {
    let query = r#"
          query Post($postId: ID!) {
            post(id: $postId) {
              id
              title
              body
            }
          }
        "#;

    let (response, server) = execute(query, map!({ "postId": "1" })).await;

    insta::assert_json_snapshot!(response, @r###"
        {
          "data": {
            "post": {
              "id": 1,
              "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
              "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto"
            }
          }
        }
        "###);

    req_asserts::matches(
        &server.received_requests().await.unwrap(),
        vec![Matcher::new().method("GET").path("/posts/1")],
    );
}

#[tokio::test]
async fn query_3() {
    let query = r#"
          query PostWithAuthor($postId: ID!) {
            post(id: $postId) {
              id
              title
              body
              author {
                id
                name
              }
            }
          }
      "#;

    let (response, server) = execute(query, map!({ "postId": "1" })).await;

    insta::assert_json_snapshot!(response, @r###"
        {
          "data": {
            "post": {
              "id": 1,
              "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
              "body": "quia et suscipit\nsuscipit recusandae consequuntur expedita et cum\nreprehenderit molestiae ut ut quas totam\nnostrum rerum est autem sunt rem eveniet architecto",
              "author": {
                "id": 1,
                "name": "Leanne Graham"
              }
            }
          }
        }
        "###);

    req_asserts::matches(
        &server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/posts/1"),
            Matcher::new().method("GET").path("/users/1"),
        ],
    );
}

#[tokio::test]
async fn query_4() {
    let query = r#"
          query PostsForUser($userId: ID!) {
            user(id: $userId) {
              id
              name
              posts {
                id
                title
                author {
                  id
                  name
                }
              }
            }
          }
      "#;

    let (response, server) = execute(query, map!({ "userId": "1" })).await;

    insta::assert_json_snapshot!(response, @r###"
        {
          "data": {
            "user": {
              "id": 1,
              "name": "Leanne Graham",
              "posts": [
                {
                  "id": 1,
                  "title": "sunt aut facere repellat provident occaecati excepturi optio reprehenderit",
                  "author": {
                    "id": 1,
                    "name": "Leanne Graham"
                  }
                },
                {
                  "id": 2,
                  "title": "qui est esse",
                  "author": {
                    "id": 1,
                    "name": "Leanne Graham"
                  }
                }
              ]
            }
          }
        }
        "###);

    req_asserts::matches(
        &server.received_requests().await.unwrap(),
        vec![
            Matcher::new().method("GET").path("/users/1"),
            Matcher::new().method("GET").path("/users/1/posts"),
            Matcher::new().method("GET").path("/posts/1"),
            Matcher::new().method("GET").path("/posts/2"),
            Matcher::new().method("GET").path("/users/1"),
        ],
    );
}
