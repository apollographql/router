import "dotenv/config";
import ld from "@launchdarkly/node-server-sdk";

const {
  LAUNCH_DARKLY_PROJECT_ID,
  LAUNCH_DARKLY_ENVIRONMENT,
  LAUNCH_DARKLY_REST_API_KEY,
  LAUNCH_DARKLY_SDK_KEY,
} = process.env;

export async function listenForFlagUpdates(
  listener: (name: string, value: number) => void,
) {
  if (
    !LAUNCH_DARKLY_SDK_KEY ||
    !LAUNCH_DARKLY_REST_API_KEY ||
    !LAUNCH_DARKLY_PROJECT_ID ||
    !LAUNCH_DARKLY_ENVIRONMENT
  ) {
    throw new Error(
      "Missing required environment variables, please check your .env file",
    );
  }
  const ldClient = ld.init(LAUNCH_DARKLY_SDK_KEY);
  await ldClient.waitForInitialization();

  const allFlagsResult = await (
    await fetch(
      `https://app.launchdarkly.com/api/v2/flags/${LAUNCH_DARKLY_PROJECT_ID}?env=${LAUNCH_DARKLY_ENVIRONMENT}`,
      {
        headers: {
          Authorization: LAUNCH_DARKLY_REST_API_KEY,
        },
      },
    )
  ).json();

  for (const flag of allFlagsResult.items) {
    const ffKey = flag.key;
    const variations =
      flag.environments[LAUNCH_DARKLY_ENVIRONMENT]._summary.variations;
    if (Object.keys(variations).length === 2 && variations["0"].rollout) {
      listener(ffKey, variations["0"].rollout / 1000);
    }
  }

  ldClient.on("update", async (param) => {
    const updatedFlag = await (
      await fetch(
        `https://app.launchdarkly.com/api/v2/flags/${LAUNCH_DARKLY_PROJECT_ID}/${param.key}?env=${LAUNCH_DARKLY_ENVIRONMENT}`,
        {
          headers: {
            Authorization: LAUNCH_DARKLY_REST_API_KEY,
          },
        },
      )
    ).json();
    listener(
      param.key,
      updatedFlag.environments[LAUNCH_DARKLY_ENVIRONMENT].fallthrough.rollout
        .variations[0].weight / 1000,
    );
  });
}
