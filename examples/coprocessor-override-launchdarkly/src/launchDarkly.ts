const {
  LAUNCH_DARKLY_PROJECT_ID,
  LAUNCH_DARKLY_ENVIRONMENT,
  LAUNCH_DARKLY_REST_API_KEY,
} = process.env;

export async function fetchFlagValues() {
  if (!LAUNCH_DARKLY_PROJECT_ID || !LAUNCH_DARKLY_ENVIRONMENT || !LAUNCH_DARKLY_REST_API_KEY) {
    throw new Error("Missing required environment variables, please check your .env file");
  }

  const result = await fetch(`https://app.launchdarkly.com/api/v2/flags/${LAUNCH_DARKLY_PROJECT_ID}`, {
    headers: {
      'Authorization': LAUNCH_DARKLY_REST_API_KEY,
    }
  });

  return Object.fromEntries((await result.json()).items.map((item: any) => {
    const ffKey = item.key;
    const variations = item.environments[LAUNCH_DARKLY_ENVIRONMENT]._summary.variations;
    if (Object.keys(variations).length !== 2) {
      console.log(`Flag ${ffKey} has ${Object.keys(variations).length} variations, but we only support 2`);
      return null;
    } else {
      if (!variations['0'].rollout) return null;
      return [ffKey, variations['0'].rollout / 1000];
    }
  }).filter(Boolean));
}