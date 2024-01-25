import express from "express";
import { fetchFlagValues } from "./launchDarkly.js";

const LABEL_PREFIX = "launchDarkly:";
const UNRESOLVED_LABELS_CONTEXT_KEY = "apollo_override::unresolved_labels";
const LABELS_TO_OVERRIDE_CONTEXT_KEY = "apollo_override::labels_to_override";

const { PORT, LAUNCH_DARKLY_POLL_INTERVAL } = process.env;

const port = PORT ? parseInt(PORT) : 3000;
const pollInterval = LAUNCH_DARKLY_POLL_INTERVAL
  ? parseInt(LAUNCH_DARKLY_POLL_INTERVAL)
  : 60000;

let flagValues: Record<string, string> = await fetchFlagValues();
setInterval(async () => {
  flagValues = await fetchFlagValues();
}, pollInterval);

const app = express();
app.use(express.json());
app.post("/", async (req, res) => {
  const { context, ...rest } = req.body;
  const unresolvedLabels: string[] = context.entries[UNRESOLVED_LABELS_CONTEXT_KEY] || [];

  const labelsToOverride = unresolvedLabels.filter((label) => {
    // ignore labels that don't start with our prefix
    if (!label.startsWith(LABEL_PREFIX)) return false;
    // remove prefix from label
    const flagKey = label.substring(LABEL_PREFIX.length);

    // find flagKey in flagValues and roll the dice to see if we should override
    const flagValue = flagValues[flagKey];
    if (!flagValue) return false;
    return Math.random() * 100 < parseFloat(flagValue);
  });

  context.entries[LABELS_TO_OVERRIDE_CONTEXT_KEY] = [
    ...(context.entries[LABELS_TO_OVERRIDE_CONTEXT_KEY] ?? []),
    ...labelsToOverride,
  ];

  res.json({ context, ...rest });
});

app.listen(port);
