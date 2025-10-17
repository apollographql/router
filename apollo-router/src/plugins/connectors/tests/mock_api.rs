struct PathTemplate(String);

impl wiremock::Match for PathTemplate {
    fn matches(&self, request: &wiremock::Request) -> bool {
        let path = request.url.path();
        let path = path.split('/');
        let template = self.0.split('/');

        for pair in path.zip_longest(template) {
            match pair {
                EitherOrBoth::Both(p, t) => {
                    if t.starts_with('{') && t.ends_with('}') {
                        continue;
                    }

                    if p != t {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }
}

#[allow(dead_code)]
fn path_template(template: &str) -> PathTemplate {
    PathTemplate(template.to_string())
}

use super::*;

pub(crate) fn users() -> Mock {
    Mock::given(method("GET")).and(path("/users")).respond_with(
        ResponseTemplate::new(200).set_body_json(serde_json::json!([
          {
            "id": 1,
            "name": "Leanne Graham"
          },
          {
            "id": 2,
            "name": "Ervin Howell",
          }
        ])),
    )
}

pub(crate) fn user_2_nicknames() -> Mock {
    Mock::given(method("GET"))
        .and(path("/users/2/nicknames"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(["cat"])))
}

pub(crate) fn users_error() -> Mock {
    Mock::given(method("GET")).and(path("/users")).respond_with(
        ResponseTemplate::new(404).set_body_json(serde_json::json!([
            {
                "kind": "json",
                "content": {},
                "selection": null
            }
        ])),
    )
}

pub(crate) fn user_1() -> Mock {
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "id": 1,
          "name": "Leanne Graham",
          "username": "Bret",
          "phone": "1-770-736-8031 x56442",
          "email": "Sincere@april.biz",
          "website": "hildegard.org"
        })))
}

pub(crate) fn user_2() -> Mock {
    Mock::given(method("GET"))
        .and(path("/users/2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "id": 2,
          "name": "Ervin Howell",
          "username": "Antonette",
          "phone": "1-770-736-8031 x56442",
          "email": "Shanna@melissa.tv",
          "website": "anastasia.net"
        })))
}

pub(crate) fn create_user() -> Mock {
    Mock::given(method("POST")).and(path("/user")).respond_with(
        ResponseTemplate::new(200).set_body_json(serde_json::json!(
          {
            "id": 3,
            "username": "New User"
          }
        )),
    )
}

pub(crate) fn user_1_with_pet() -> Mock {
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
          "id": 1,
          "name": "Leanne Graham",
          "pet": {
              "name": "Spot"
          }
        })))
}

pub(crate) fn commits() -> Mock {
    Mock::given(method("GET"))
        .and(path("/repos/foo/bar/commits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
          [
            {
              "sha": "abcdef",
              "commit": {
                "author": {
                  "name": "Foo Bar",
                  "email": "noone@nowhere",
                  "date": "2024-07-09T01:22:33Z"
                },
                "message": "commit message",
              },
            }]
        )))
}

pub(crate) fn posts() -> Mock {
    Mock::given(method("GET")).and(path("/posts")).respond_with(
        ResponseTemplate::new(200).set_body_json(serde_json::json!([
          {
            "id": 1,
            "title": "Post 1",
            "userId": 1
          },
          {
            "id": 2,
            "title": "Post 2",
            "userId": 2
          }
        ])),
    )
}
