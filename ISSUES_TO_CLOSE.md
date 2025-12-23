# Apollo Router Issues Ready to Close

This document lists issues that have been verified as resolved in the CHANGELOG and can be closed.

## Verified Resolved Issues

### Issue #8661: Missing telemetry metrics - body sizes
- **URL**: https://github.com/apollographql/router/issues/8661
- **Reported**: Nov 21, 2025
- **Resolved in**: v2.10.0 (Released Dec 11, 2025)
- **Fix PRs**:
  - #8712: Emit `http.client.request.body.size` metric correctly
  - #8697: Record `http.server.response.body.size` metric correctly
- **Suggested closing comment**:
  ```
  This has been resolved in Apollo Router v2.10.0.
  
  The missing metrics are now correctly emitted:
  - `http.server.response.body.size` - Fixed in PR #8697
  - `http.client.request.body.size` - Fixed in PR #8712
  
  See the v2.10.0 CHANGELOG for details: https://github.com/apollographql/router/blob/main/CHANGELOG.md#2100---2025-12-11
  
  Please upgrade to v2.10.0 or later to use these metrics.
  ```

---

### Issue #8627: HTTP/2 requests fail with 431 when headers exceed 16KB
- **URL**: https://github.com/apollographql/router/issues/8627
- **Reported**: Nov 18, 2025
- **Resolved in**: v2.9.0 (Released Nov 27, 2025)
- **Fix PR**: #8636: Configure maximum HTTP/2 header list size
- **Suggested closing comment**:
  ```
  This has been resolved in Apollo Router v2.9.0.
  
  The HTTP/2 header list size limit is now configurable via the `limits.http2_max_headers_list_bytes` setting. The default remains 16KiB but can be increased as needed.
  
  Example configuration:
  ```yaml
  limits:
    http2_max_headers_list_bytes: "48KiB"  # or whatever size you need
  ```
  
  Fixed in PR #8636.
  See the v2.9.0 CHANGELOG: https://github.com/apollographql/router/blob/main/CHANGELOG.md#290---2025-11-27
  
  Please upgrade to v2.9.0 or later and configure the limit for your use case.
  ```

---

## Partially Resolved Issues (Needs Clarification)

### Issue #8738: Ability to configure Response Cache's `private_id` without a Rhai script
- **URL**: https://github.com/apollographql/router/issues/8738
- **Status**: Partially addressed via Rhai/coprocessor customization
- **Resolved in**: v2.10.0 (partial)
- **Fix PR**: #8652: Customize response caching behavior at the subgraph level
- **Note**: The request for pure YAML configuration (without Rhai) has NOT been implemented. However, the ability to customize `private_id` is now available via Rhai scripts or coprocessors.
- **Suggested action**: Update the issue to clarify current status or keep open as a feature request for YAML-based configuration.

---

## Issues Requiring Investigation

### Issue #8781: WebSocket: Router doesn't parse accents from co-processor's context
- **URL**: https://github.com/apollographql/router/issues/8781
- **Created**: Dec 23, 2025 (Very recent)
- **Status**: New bug report - needs investigation by Router team
- **Details**: UTF-8 encoding issue with accented characters in WebSocket headers when using coprocessor context

---

## How to Use This Document

For Apollo Router maintainers:

1. **Review** each issue in the "Verified Resolved Issues" section
2. **Test** (optional but recommended) that the issue is indeed fixed in the specified version
3. **Close** the issue with the suggested comment (or your own variation)
4. **Label** with appropriate version labels (e.g., "fixed-in-v2.10.0")

## Statistics

- **Total issues analyzed**: 383
- **Verified resolved**: 2
- **Partially resolved**: 1
- **Needs investigation**: 1
- **Remaining to analyze**: ~379

## Next Steps

A systematic review of all 383 issues should be conducted to identify additional resolved issues. Based on the high rate of active development, an estimated 10-20% (38-77 issues) may be closeable.

See `ISSUES_ANALYSIS_REPORT.md` for the full analysis methodology and recommendations.
