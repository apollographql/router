//! Canned data for use with the canned schema (`/testing_schema.graphl`).
use serde_json::json;

pub(crate) fn mock_subgraphs() -> serde_json::Value {
    json!({
        "accounts": {
            "entities": [
                { "__typename": "User", "id": "1", "name": "Ada Lovelace" },
                { "__typename": "User", "id": "2", "name": "Alan Turing" },
            ],
        },
        "products": {
            "query": {
                "topProducts": [
                    { "__typename": "Product", "upc": "1", "name": "Table" },
                    { "__typename": "Product", "upc": "2", "name": "Couch" },
                ],
            },
            "entities": [
                { "__typename": "Product", "upc": "1", "name": "Table" },
                { "__typename": "Product", "upc": "2", "name": "Couch" },
            ],
        },
        "reviews": {
            "entities": [
                {
                    "__typename": "Product",
                    "upc": "1",
                    "reviews": [
                        {
                            "__typename": "Review",
                            "id": "1",
                            "product": { "__typename": "Product", "upc": "1" },
                            "author": { "__typename": "User", "id": "1" },
                        },
                        {
                            "__typename": "Review",
                            "id": "4",
                            "product": { "__typename": "Product", "upc": "1" },
                            "author": { "__typename": "User", "id": "2" },
                        },
                    ],
                },
                {
                    "__typename": "Product",
                    "upc": "2",
                    "reviews": [
                        {
                            "__typename": "Review",
                            "id": "2",
                            "product": { "__typename": "Product", "upc": "2" },
                            "author": { "__typename": "User", "id": "1" },
                        },
                    ],
                },
                {
                    "__typename": "Review",
                    "id": "1",
                    "product": { "__typename": "Product", "upc": "1" },
                    "author": { "__typename": "User", "id": "1" },
                },
                {
                    "__typename": "Review",
                    "id": "2",
                    "product": { "__typename": "Product", "upc": "2" },
                    "author": { "__typename": "User", "id": "1" },
                },
                {
                    "__typename": "Review",
                    "id": "4",
                    "product": { "__typename": "Product", "upc": "1" },
                    "author": { "__typename": "User", "id": "2" },
                },
            ],
        },
    })
}
