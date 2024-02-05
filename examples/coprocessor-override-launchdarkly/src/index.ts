import "dotenv/config";
import express from "express";
import { listenForFlagUpdates } from "./launchDarkly.js";

const LABEL_PREFIX = "launchDarkly:";
const UNRESOLVED_LABELS_CONTEXT_KEY = "apollo_override::unresolved_labels";
const LABELS_TO_OVERRIDE_CONTEXT_KEY = "apollo_override::labels_to_override";

const { PORT } = process.env;

const port = PORT ? parseInt(PORT) : 3000;

let flagValues: Record<string, number> = {};
listenForFlagUpdates((name, value) => {
  flagValues[name] = value;
});

const app = express();
app.use(express.json());
app.post("/", async (req, res) => {
  const { context, ...rest } = req.body;
  const unresolvedLabels: string[] =
    context.entries[UNRESOLVED_LABELS_CONTEXT_KEY] || [];

  const labelsToOverride = unresolvedLabels.filter((label) => {
    // ignore labels that don't start with our prefix
    if (!label.startsWith(LABEL_PREFIX)) return false;
    // remove prefix from label
    const flagKey = label.substring(LABEL_PREFIX.length);

    // find flagKey in flagValues and roll the dice to see if we should override
    const flagValue = flagValues[flagKey];
    if (!flagValue) return false;
    return Math.random() * 100 < flagValue;
  });

  context.entries[LABELS_TO_OVERRIDE_CONTEXT_KEY] = [
    ...(context.entries[LABELS_TO_OVERRIDE_CONTEXT_KEY] ?? []),
    ...labelsToOverride,
  ];

  res.json({ context, ...rest });
});

app.listen(port, () => {
  console.log(`Listening on port ${port}`);
});
