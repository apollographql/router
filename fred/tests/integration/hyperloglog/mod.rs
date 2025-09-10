use fred::prelude::*;

pub async fn should_pfadd_elements(client: Client, _: Config) -> Result<(), Error> {
  let result: i64 = client.pfadd("foo", vec!["a", "b"]).await?;
  assert_eq!(result, 1);
  let result: i64 = client.pfadd("foo", "a").await?;
  assert_eq!(result, 0);

  Ok(())
}

pub async fn should_pfcount_elements(client: Client, _: Config) -> Result<(), Error> {
  let result: i64 = client.pfadd("foo", vec!["a", "b", "c"]).await?;
  assert_eq!(result, 1);
  let result: i64 = client.pfcount("foo").await?;
  assert_eq!(result, 3);
  let result: i64 = client.pfadd("foo", vec!["c", "d", "e"]).await?;
  assert_eq!(result, 1);
  let result: i64 = client.pfcount("foo").await?;
  assert_eq!(result, 5);

  Ok(())
}

pub async fn should_pfmerge_elements(client: Client, _: Config) -> Result<(), Error> {
  let result: i64 = client.pfadd("foo{1}", vec!["a", "b", "c"]).await?;
  assert_eq!(result, 1);
  let result: i64 = client.pfadd("bar{1}", vec!["c", "d", "e"]).await?;
  assert_eq!(result, 1);

  let _: () = client.pfmerge("baz{1}", vec!["foo{1}", "bar{1}"]).await?;
  let result: i64 = client.pfcount("baz{1}").await?;
  assert_eq!(result, 5);

  Ok(())
}
