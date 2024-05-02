pub(crate) struct Test {}

impl Test {
    pub(crate) fn from_configuration(config: &crate::configuration::Cache) {
        println!("config {:?}", config)
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn testing() {
        Test::from_configuration(crate::configuration::Cache::new())
    }
}
