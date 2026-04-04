/**
 * Data Access Layer for Apollo Router Diagnostics
 *
 * Provides a unified interface for loading diagnostic data in both dashboard
 * and embedded modes. Handles switching between live API endpoints and embedded
 * base64-encoded data.
 *
 * ## Architecture
 *
 * Two modes of operation:
 * - **Dashboard Mode** (IS_DASHBOARD_MODE=true): Fetches data from REST API endpoints
 * - **Embedded Mode** (IS_DASHBOARD_MODE=false): Decodes data from EMBEDDED_DATA object
 *
 * ## Data Sources
 *
 * Loads the following diagnostic data:
 * - System information (OS, CPU, memory)
 * - Router configuration (YAML)
 * - Supergraph schema (GraphQL)
 * - Memory heap dumps (.prof files)
 *
 * ## Security
 *
 * All data is fetched from same-origin endpoints or embedded at build time.
 * No external data sources are accessed to prevent CORS and XSS issues.
 *
 * @module data-access
 */

// ===== Utility Functions =====

function base64Decode(str) {
    try {
        return atob(str);
    } catch (e) {
        console.error('Failed to decode base64:', e);
        return str;
    }
}

// ===== Configuration =====

// Data loading configuration
const DATA_CONFIG = {
    useLocalFiles: false, // Set to false to use embedded data
    basePath: './', // Base path for data files
    files: {
        systemInfo: 'report.txt',
        routerConfig: 'router.yaml',
        schema: 'supergraph.graphql',
        memoryDumps: [
            'memory/router_heap_dump_1757588839.prof',
            'memory/router_heap_dump_1757588881.prof'
        ]
    }
};

// API base URL (absolute path)
const API_BASE = '/diagnostics/';

// ===== File Loading Functions =====

async function loadTextFile(filepath) {
    try {
        const response = await fetch(DATA_CONFIG.basePath + filepath);
        if (!response.ok) {
            throw new Error(`HTTP error! status: ${response.status}`);
        }
        return await response.text();
    } catch (error) {
        console.error(`Failed to load ${filepath}:`, error);
        return `Error: Could not load ${filepath}`;
    }
}

async function loadBinaryFile(filepath) {
    try {
        const response = await fetch(DATA_CONFIG.basePath + filepath);
        if (!response.ok) {
            throw new Error(`HTTP error! status: ${response.status}`);
        }
        const arrayBuffer = await response.arrayBuffer();
        return new Uint8Array(arrayBuffer);
    } catch (error) {
        console.error(`Failed to load ${filepath}:`, error);
        return `Error: Could not load ${filepath}`;
    }
}

// ===== Main Data Loading Functions =====

async function loadAllData() {
    if (DATA_CONFIG.useLocalFiles) {
        try {
            // Load text files
            const [systemInfo, routerConfig, schema] = await Promise.all([
                loadTextFile(DATA_CONFIG.files.systemInfo),
                loadTextFile(DATA_CONFIG.files.routerConfig),
                loadTextFile(DATA_CONFIG.files.schema)
            ]);

            // Load memory dumps
            const memoryDumps = [];
            for (let i = 0; i < DATA_CONFIG.files.memoryDumps.length; i++) {
                const filepath = DATA_CONFIG.files.memoryDumps[i];
                const data = await loadBinaryFile(filepath);
                if (data) {
                    const filename = filepath.split('/').pop();
                    memoryDumps.push({
                        name: filename,
                        data: data,
                        size: data.length,
                        path: filepath
                    });
                }
            }

            // Update global data
            window.LOADED_DATA = {
                systemInfo: systemInfo,
                routerConfig: routerConfig,
                schema: schema,
                memoryDumps: memoryDumps
            };

            return window.LOADED_DATA;

        } catch (error) {
            console.error('Failed to load data:', error);
            // Fallback to embedded data
            return await loadEmbeddedData();
        }
    } else {
        // Check if we have usable embedded data (static report mode) or should use API (dashboard mode)
        if (typeof EMBEDDED_DATA !== 'undefined' && EMBEDDED_DATA &&
            (EMBEDDED_DATA.systemInfo || EMBEDDED_DATA.routerConfig || EMBEDDED_DATA.schema)) {
            return await loadEmbeddedData();
        } else {
            // Dashboard mode - fetch from API endpoints
            return await loadApiData();
        }
    }
}

async function loadEmbeddedData() {
    // Decode any base64 encoded data in embedded mode
    const decodedData = {
        systemInfo: EMBEDDED_DATA.systemInfo ?
            (typeof EMBEDDED_DATA.systemInfo === 'string' && EMBEDDED_DATA.systemInfo.match(/^[A-Za-z0-9+/]+=*$/) ?
                base64Decode(EMBEDDED_DATA.systemInfo) : EMBEDDED_DATA.systemInfo) : null,
        routerConfig: EMBEDDED_DATA.routerConfig ?
            (typeof EMBEDDED_DATA.routerConfig === 'string' && EMBEDDED_DATA.routerConfig.match(/^[A-Za-z0-9+/]+=*$/) ?
                base64Decode(EMBEDDED_DATA.routerConfig) : EMBEDDED_DATA.routerConfig) : null,
        schema: EMBEDDED_DATA.schema ?
            (typeof EMBEDDED_DATA.schema === 'string' && EMBEDDED_DATA.schema.match(/^[A-Za-z0-9+/]+=*$/) ?
                base64Decode(EMBEDDED_DATA.schema) : EMBEDDED_DATA.schema) : null,
        memoryDumps: EMBEDDED_DATA.memoryDumps || []
    };

    // Set global data
    window.LOADED_DATA = decodedData;
    return decodedData;
}

async function loadApiData() {
    try {
        // Fetch data from API endpoints
        const [systemInfoResponse, routerConfigResponse, schemaResponse, dumpsResponse] = await Promise.all([
            fetch(API_BASE + 'report.txt'),
            fetch(API_BASE + 'router_config.yaml'),
            fetch(API_BASE + 'supergraph.graphql'),
            fetch(API_BASE + 'memory/dumps')
        ]);

        // Get text content
        const [systemInfo, routerConfig, schema] = await Promise.all([
            systemInfoResponse.text(),
            routerConfigResponse.text(),
            schemaResponse.text()
        ]);

        // Get memory dumps
        const dumps = dumpsResponse.ok ? await dumpsResponse.json() : [];

        // Set global data for other functions to use
        const loadedData = {
            systemInfo: systemInfo,
            routerConfig: routerConfig,
            schema: schema,
            memoryDumps: dumps
        };

        window.LOADED_DATA = loadedData;
        return loadedData;

    } catch (error) {
        console.error('Failed to load data from API:', error);
        throw error;
    }
}

/**
 * Fetch static router system info (JSON).
 * Returns null if endpoint is unavailable (e.g. router not fully started).
 */
async function fetchRouterSystemInfo() {
    try {
        const response = await fetch(API_BASE + 'system_info');
        if (!response.ok) return null;
        return await response.json();
    } catch (error) {
        console.error('Failed to fetch router system info:', error);
        return null;
    }
}

/**
 * Fetch router system info and return minimal summary for dashboard home (version, OS, arch, config/supergraph one-liner).
 */
async function fetchRouterSystemInfoSummary() {
    const data = await fetchRouterSystemInfo();
    return data ? formatRouterSystemInfoSummary(data) : null;
}

/**
 * Fetch router system info and return full formatted text for System info tab / copy-paste.
 */
async function fetchRouterSystemInfoFormatted() {
    const data = await fetchRouterSystemInfo();
    return data ? formatRouterSystemInfoForDisplay(data) : null;
}

/**
 * Minimal summary for dashboard home: version, OS/arch, config and supergraph one-liner.
 */
function formatRouterSystemInfoSummary(data) {
    const lines = [];
    lines.push('Router version: ' + (data.version || ''));
    lines.push('OS / architecture: ' + (data.os || '') + ' / ' + (data.arch || '') + (data.target_family ? ' (' + data.target_family + ')' : ''));
    const config = data.config_path ? 'path=' + data.config_path + (data.config_hash ? ' hash=' + data.config_hash : '') : 'default or not from file';
    const supergraph = data.supergraph_source ? 'source=' + data.supergraph_source + (data.supergraph_hash ? ' hash=' + data.supergraph_hash : '') : '(not set)';
    lines.push('Config: ' + config + ' · Supergraph: ' + supergraph);
    return lines.join('\n');
}

/**
 * Format RouterSystemInfo JSON into copy-paste friendly text (full detail for System info tab).
 * Lists every static option (flags and env) whether set or not, so the UI matches the JSON.
 */
function formatRouterSystemInfoForDisplay(data) {
    const lines = [];
    lines.push('Router version: ' + (data.version || ''));
    lines.push('OS / architecture: ' + (data.os || '') + ' / ' + (data.arch || '') + ' (' + (data.target_family || '') + ')');
    lines.push('');
    lines.push('Startup options (flags / env):');
    const opts = data.startup_options || {};
    lines.push('  --log: ' + (opts.log_level != null && opts.log_level !== '' ? opts.log_level : '(not set)'));
    lines.push('  --hot-reload: ' + (opts.hot_reload ? 'yes' : 'no'));
    lines.push('  --dev: ' + (opts.dev ? 'yes' : 'no'));
    lines.push('  --listen: ' + (opts.listen_address != null && opts.listen_address !== '' ? opts.listen_address : '(not set)'));
    lines.push('  --config: ' + (opts.config_path !== undefined && opts.config_path !== null ? '(set)' : '(not set)'));
    lines.push('  --supergraph: ' + (opts.supergraph_path !== undefined && opts.supergraph_path !== null ? '(set)' : '(not set)'));
    lines.push('  --supergraph-urls: ' + (opts.supergraph_urls !== undefined && opts.supergraph_urls !== null ? '(set)' : '(not set)'));
    lines.push('  APOLLO_KEY: ' + (opts.apollo_key_set ? '(set)' : '(not set)'));
    lines.push('  APOLLO_GRAPH_REF: ' + (opts.apollo_graph_ref_set ? '(set)' : '(not set)'));
    lines.push('  APOLLO_ROUTER_LICENSE: ' + (opts.apollo_router_license_set ? '(set)' : '(not set)'));
    lines.push('  APOLLO_ROUTER_LICENSE_PATH: ' + (opts.apollo_router_license_path_set ? '(set)' : '(not set)'));
    lines.push('  APOLLO_GRAPH_ARTIFACT_REFERENCE: ' + (opts.graph_artifact_reference_set ? '(set)' : '(not set)'));
    lines.push('  APOLLO_TELEMETRY_DISABLED: ' + (opts.anonymous_telemetry_disabled ? '(set)' : '(not set)'));
    lines.push('');
    lines.push('Config file: ' + (data.config_path ? 'path=' + data.config_path + (data.config_hash ? ' hash=' + data.config_hash : ' (hash not available)') : '(default or not from file)'));
    lines.push('Supergraph: ' + (data.supergraph_source ? 'source=' + data.supergraph_source + (data.supergraph_hash ? ' hash=' + data.supergraph_hash : ' (hash not available)') : '(not set)'));
    lines.push('Environment variables set: ' + (data.set_env_var_names && data.set_env_var_names.length ? data.set_env_var_names.join(', ') : '(none)'));
    return lines.join('\n');
}

// ===== Dashboard API Functions =====

async function fetchProfilingStatus() {
    try {
        const response = await fetch(API_BASE + 'memory/status');
        const data = await response.json();
        return data;
    } catch (error) {
        console.error('Failed to fetch profiling status:', error);
        throw error;
    }
}

async function startProfiling() {
    try {
        const response = await fetch(API_BASE + 'memory/start', {method: 'POST'});
        const data = await response.json();

        if (!response.ok) {
            throw new Error(data.message || 'Failed to start profiling');
        }

        return data;
    } catch (error) {
        console.error('Failed to start profiling:', error);
        throw error;
    }
}

async function stopProfiling() {
    try {
        const response = await fetch(API_BASE + 'memory/stop', {method: 'POST'});
        const data = await response.json();

        if (!response.ok) {
            throw new Error(data.message || 'Failed to stop profiling');
        }

        return data;
    } catch (error) {
        console.error('Failed to stop profiling:', error);
        throw error;
    }
}

async function triggerDump() {
    try {
        const response = await fetch(API_BASE + 'memory/dump', {method: 'POST'});
        const data = await response.json();

        if (!response.ok) {
            throw new Error(data.message || 'Failed to trigger dump');
        }

        return data;
    } catch (error) {
        console.error('Failed to trigger dump:', error);
        throw error;
    }
}

async function listDumps() {
    try {
        const response = await fetch(API_BASE + 'memory/dumps');
        const dumps = await response.json();
        return dumps;
    } catch (error) {
        console.error('Failed to list dumps:', error);
        throw error;
    }
}

async function downloadDump(filename) {
    try {
        const response = await fetch(API_BASE + `memory/dumps/${filename}`);
        if (!response.ok) {
            throw new Error(`Failed to download dump: ${response.status}`);
        }

        const blob = await response.blob();
        return blob;
    } catch (error) {
        console.error('Failed to download dump:', error);
        throw error;
    }
}

async function deleteDump(filename) {
    try {
        const response = await fetch(API_BASE + `memory/dumps/${filename}`, {method: 'DELETE'});
        const data = await response.json();

        if (!response.ok) {
            throw new Error(data.message || 'Failed to delete dump');
        }

        return data;
    } catch (error) {
        console.error('Failed to delete dump:', error);
        throw error;
    }
}

async function clearAllDumps() {
    try {
        const response = await fetch(API_BASE + 'memory/dumps', {method: 'DELETE'});
        const data = await response.json();

        if (!response.ok) {
            throw new Error(data.message || 'Failed to clear dumps');
        }

        return data;
    } catch (error) {
        console.error('Failed to clear dumps:', error);
        throw error;
    }
}

async function exportDiagnostics() {
    try {
        const response = await fetch(API_BASE + 'export');

        if (!response.ok) {
            const errorData = await response.json();
            throw new Error(errorData.message || 'Export failed');
        }

        const blob = await response.blob();
        return blob;
    } catch (error) {
        console.error('Failed to export diagnostics:', error);
        throw error;
    }
}

// ===== Centralized Data Access Layer =====

// Centralized data access - abstracts away embedded vs API mode
const DataAccess = {
    // Fetch minimal router system info for dashboard summary card
    async fetchRouterSystemInfoSummary() {
        return fetchRouterSystemInfoSummary();
    },
    // Fetch full formatted router system info for System info tab
    async fetchRouterSystemInfoFormatted() {
        return fetchRouterSystemInfoFormatted();
    },

    // Get memory dumps list
    async getMemoryDumps() {
        if (this.isDashboardMode()) {
            // Dashboard mode - fetch from API
            try {
                const response = await fetch(API_BASE + 'memory/dumps');
                return await response.json();
            } catch (error) {
                console.error('Failed to fetch dumps:', error);
                return [];
            }
        } else {
            // Embedded mode - use loaded data
            return window.LOADED_DATA?.memoryDumps || [];
        }
    },

    // Get memory dump content by name (handles base64 decoding internally)
    async getMemoryDump(dumpName) {
        let dump;
        if (this.isDashboardMode()) {
            // Dashboard mode - fetch from API
            try {
                const response = await fetch(API_BASE + `memory/dumps/${dumpName}`);
                if (!response.ok) {
                    throw new Error(`Failed to fetch dump: ${response.status}`);
                }
                const dumpContent = await response.text();
                dump = {name: dumpName, data: dumpContent};
            } catch (error) {
                console.error('Failed to fetch dump:', error);
                throw error;
            }
        } else {
            // Embedded mode - use loaded data
            const dumps = window.LOADED_DATA?.memoryDumps || [];
            dump = dumps.find(d => d.name === dumpName);
            if (!dump) {
                throw new Error(`Dump not found: ${dumpName}`);
            }
        }

        // Handle base64 decoding if needed (centralized here)
        if (typeof dump.data === 'string' && dump.data.match(/^[A-Za-z0-9+/]+=*$/)) {
            return {
                ...dump,
                data: base64Decode(dump.data)
            };
        }

        return dump;
    },

    // Check if we're in dashboard mode (vs embedded mode)
    isDashboardMode() {
        // First check for explicit dashboard mode flag (most reliable)
        if (typeof IS_DASHBOARD_MODE !== 'undefined') {
            return IS_DASHBOARD_MODE;
        }

        // Fallback: If we have embedded data with actual content, we're definitely in static export mode
        if (typeof EMBEDDED_DATA !== 'undefined' && EMBEDDED_DATA &&
            (EMBEDDED_DATA.systemInfo || EMBEDDED_DATA.routerConfig || EMBEDDED_DATA.schema || EMBEDDED_DATA.memoryDumps)) {
            return false;
        }

        // If useLocalFiles is true, we're in file-based development mode (not dashboard mode)
        if (typeof DATA_CONFIG !== 'undefined' && DATA_CONFIG && DATA_CONFIG.useLocalFiles === true) {
            return false;
        }

        // Otherwise, we're in dashboard mode (live API mode)
        return true;
    }
};