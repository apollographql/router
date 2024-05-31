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
    async fn me(&self, _ctx: &Context<'_>) -> User {
        /*let random = &mut StdRng::seed_from_u64(3974).sample_iter(&Alphanumeric);
        let mut result = Vec::with_capacity(limit);
        for count in 0..limit {
            let data = random.take(size.unwrap_or(1)).map(char::from).collect();
            result.push(product_for_upc(count.to_string(), data))
        }
        result*/
        todo!()
    }

    async fn topProducts(
        &self,
        _ctx: &Context<'_>,
        #[graphql(desc = "number of products to return")] first: Option<usize>,
    ) -> Vec<Product> {
        /*let random = &mut StdRng::seed_from_u64(3974).sample_iter(&Alphanumeric);
        let mut result = Vec::with_capacity(limit);
        for count in 0..limit {
            let data = random.take(size.unwrap_or(1)).map(char::from).collect();
            result.push(product_for_upc(count.to_string(), data))
        }
        result*/
        todo!()
    }

    #[graphql(entity)]
    async fn find_user_by_id(&self, id: String) -> User {
        //product_for_upc(upc, "1".to_string())
        todo!()
    }

    #[graphql(entity)]
    async fn find_product_by_upc(&self, upc: String) -> Product {
        //product_for_upc(upc, "1".to_string())
        todo!()
    }

    #[graphql(entity)]
    async fn find_review_by_id(&self, id: String) -> Review {
        //product_for_upc(upc, "1".to_string())
        todo!()
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
    async fn topProducts(
        &self,
        _ctx: &Context<'_>,
        #[graphql(desc = "number of products to return")] first: Option<usize>,
    ) -> Vec<Product> {
        /*let random = &mut StdRng::seed_from_u64(3974).sample_iter(&Alphanumeric);
        let mut result = Vec::with_capacity(limit);
        for count in 0..limit {
            let data = random.take(size.unwrap_or(1)).map(char::from).collect();
            result.push(product_for_upc(count.to_string(), data))
        }
        result*/
        todo!()
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
    reviews: Vec<Review>,
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

    async fn reviews(&self) -> &[Review] {
        /*Review {
            body: "1".to_string(),
            product: Product {
                upc: "00000000-0000-0000-0000-000000000000".to_string(),
            },
        }*/
        todo!()
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
    name: String,
    price: u32,
    weight: u32,
    inStock: bool,
    shippingEstimate: u32,
    reviews: Vec<Review>,
}

fn product_for_upc(upc: String, data: String) -> Product {
    /*Product {
        upc: "00000000-0000-0000-0000-000000000000".to_string(),
        name: upc,
        price: data,
    }*/
    todo!()
}

#[Object]
impl Product {
    async fn upc(&self) -> &String {
        &self.upc
    }

    async fn name(&self) -> &String {
        &self.name
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

    async fn shippingEstimate(&self) -> &u32 {
        &self.shippingEstimate
    }

    async fn reviews(&self) -> &[Review] {
        /*Review {
            body: "1".to_string(),
            product: Product {
                upc: "00000000-0000-0000-0000-000000000000".to_string(),
            },
        }*/
        todo!()
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
    body: String,
    author: User,
    product: Product,
}

#[Object]
impl Review {
    async fn id(&self) -> &String {
        &self.id
    }

    async fn body(&self) -> &String {
        &self.body
    }

    async fn author(&self) -> &User {
        &self.author
    }

    async fn product(&self) -> &Product {
        &self.product
    }
}
