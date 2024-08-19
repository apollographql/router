#![allow(non_snake_case)]

use std::sync::Once;

use async_graphql::Context;
use async_graphql::Object;

/*
  type Query {
    me: User
    recommendedProducts: [Product]
    topProducts(first: Int = 5): [Product]
  }
*/
pub struct Query;

#[Object]
impl Query {
    async fn me(&self, _ctx: &Context<'_>) -> Option<User> {
        users().first().cloned()
    }

    async fn topProducts(
        &self,
        _ctx: &Context<'_>,
        #[graphql(desc = "number of products to return")] first: Option<usize>,
    ) -> Vec<Product> {
        let first = first.unwrap_or(4);
        let limit = std::cmp::min(first, 2);
        let p = products();
        (p[..limit]).to_owned()
    }

    #[graphql(entity)]
    async fn find_user_by_id(&self, id: String) -> Option<User> {
        users().iter().find(|u| u.id.as_str() == id).cloned()
    }

    #[graphql(entity)]
    async fn find_product_by_upc(&self, upc: String) -> Option<Product> {
        products().iter().find(|p| p.upc.as_str() == upc).cloned()
    }

    #[graphql(entity)]
    async fn find_review_by_id(&self, id: String) -> Option<&Review> {
        reviews().iter().find(|r: &&Review| r.id.as_str() == id)
    }
}

/*
  type Mutation {
    createProduct(upc: ID!, name: String): Product
    createReview(upc: ID!, id: ID!, body: String): Review
  }
*/

pub struct Mutation;

#[Object]
impl Mutation {
    async fn createProduct(
        &self,
        _ctx: &Context<'_>,
        upc: String,
        name: Option<String>,
    ) -> Product {
        Product {
            upc,
            name,
            price: 0,
            weight: 0,
            inStock: true,
        }
    }

    async fn createReview(
        &self,
        _ctx: &Context<'_>,
        upc: String,
        id: String,
        body: Option<String>,
    ) -> Review {
        Review {
            id,
            productUpc: upc,
            body,
            authorId: "0".to_string(),
        }
    }
}

/*
  type User @key(fields: "id") {
    id: ID!
    name: String
    username: String @shareable
    reviews: [Review]
  }
*/
#[derive(Clone)]
pub struct User {
    id: String,
    name: String,
    username: String,
    //reviews: Vec<Review>,
}

#[Object]
impl User {
    async fn id(&self) -> &String {
        &self.id
    }

    async fn name(&self) -> &String {
        &self.name
    }

    async fn username(&self) -> &String {
        &self.username
    }

    async fn reviews(&self) -> Vec<&Review> {
        reviews()
            .iter()
            .filter(|r: &&Review| r.authorId.as_str() == self.id)
            .collect()
    }
}

#[allow(static_mut_refs)]
fn users() -> &'static [User] {
    static mut USERS: Vec<User> = vec![];
    static INIT: Once = Once::new();
    unsafe {
        INIT.call_once(|| {
            USERS = vec![
                User {
                    id: "1".to_string(),
                    name: "Ada Lovelace".to_string(),
                    username: "@ada".to_string(),
                },
                User {
                    id: "2".to_string(),
                    name: "Alan Turing".to_string(),
                    username: "@complete".to_string(),
                },
            ];
        });
        &USERS
    }
}

/*
  type Product @key(fields: "upc") {
    upc: String!
    name: String
    weight: Int
    price: Int
    inStock: Boolean
    shippingEstimate: Int
    reviews: [Review]
    reviewsForAuthor(authorID: ID!): [Review]
  }
*/

#[derive(Clone)]
pub struct Product {
    upc: String,
    name: Option<String>,
    price: u32,
    weight: u32,
    inStock: bool,
    //shippingEstimate: u32,
    //reviews: Vec<Review>,
}

#[Object]
impl Product {
    async fn upc(&self) -> &String {
        &self.upc
    }

    async fn name(&self) -> Option<&String> {
        self.name.as_ref()
    }

    async fn price(&self) -> &u32 {
        &self.price
    }

    async fn weight(&self) -> &u32 {
        &self.weight
    }

    async fn inStock(&self) -> bool {
        self.inStock
    }

    async fn shippingEstimate(&self) -> u32 {
        // free for expensive items
        if self.price > 1000 {
            0
        } else {
            // estimate is based on weight
            self.weight / 2
        }
    }

    async fn reviews(&self) -> Vec<Review> {
        reviews()
            .iter()
            .filter(|r: &&Review| r.productUpc.as_str() == self.upc)
            .cloned()
            .collect()
    }

    async fn reviewsForAuthor(&self, authorID: String) -> Vec<Review> {
        reviews()
            .iter()
            .filter(|r: &&Review| r.productUpc.as_str() == self.upc && r.authorId == authorID)
            .cloned()
            .collect()
    }
}

#[allow(static_mut_refs)]
fn products() -> &'static [Product] {
    static mut PRODUCTS: Vec<Product> = vec![];
    static INIT: Once = Once::new();
    unsafe {
        INIT.call_once(|| {
            PRODUCTS = vec![
                Product {
                    upc: "1".to_string(),
                    name: Some("Table".to_string()),
                    price: 899,
                    weight: 100,
                    inStock: true,
                },
                Product {
                    upc: "2".to_string(),
                    name: Some("Couch".to_string()),
                    price: 1299,
                    weight: 1000,
                    inStock: false,
                },
                Product {
                    upc: "3".to_string(),
                    name: Some("Chair".to_string()),
                    price: 54,
                    weight: 50,
                    inStock: true,
                },
                Product {
                    upc: "4".to_string(),
                    name: Some("Bed".to_string()),
                    price: 1000,
                    weight: 1200,
                    inStock: false,
                },
            ];
        });
        &PRODUCTS
    }
}

/*
  type Review @key(fields: "id") {
    id: ID!
    body: String
    author: User
    product: Product
  }
*/

#[derive(Clone)]
struct Review {
    id: String,
    body: Option<String>,
    authorId: String,
    productUpc: String,
}

#[Object]
impl Review {
    async fn id(&self) -> &String {
        &self.id
    }

    async fn body(&self) -> Option<&String> {
        self.body.as_ref()
    }

    async fn author(&self) -> Option<&User> {
        users()
            .iter()
            .find(|r: &&User| r.id.as_str() == self.authorId)
    }

    async fn product(&self) -> Option<&Product> {
        products()
            .iter()
            .find(|p| p.upc.as_str() == self.productUpc)
    }
}

#[allow(static_mut_refs)]
fn reviews() -> &'static [Review] {
    static mut REVIEWS: Vec<Review> = vec![];
    static INIT: Once = Once::new();
    unsafe {
        INIT.call_once(|| {
            REVIEWS = vec![
                Review {
                    id: "1".to_string(),
                    authorId: "1".to_string(),
                    productUpc: "1".to_string(),
                    body: Some("Love it!".to_string()),
                },
                Review {
                    id: "2".to_string(),
                    authorId: "1".to_string(),
                    productUpc: "2".to_string(),
                    body: Some("Too expensive.".to_string()),
                },
                Review {
                    id: "3".to_string(),
                    authorId: "2".to_string(),
                    productUpc: "3".to_string(),
                    body: Some("Could be better.".to_string()),
                },
                Review {
                    id: "4".to_string(),
                    authorId: "2".to_string(),
                    productUpc: "1".to_string(),
                    body: Some("Prefer something else.".to_string()),
                },
            ];
        });
        &REVIEWS
    }
}
