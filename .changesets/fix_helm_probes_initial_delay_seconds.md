### Fix initialDelaySeconds probe configuration to be applied to the correct object

Fixes the `initialDelaySeconds` configuration to be applied to the correct object (to the probe itself instead of the `httpGet` block).