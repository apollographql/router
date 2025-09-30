// Main JavaScript code for Apollo Router Diagnostics

// EMBEDDED_DATA is defined in the HTML template and will be available globally


// ===== Utility Functions =====

function base64Decode(str) {
    try {
        return atob(str);
    } catch (e) {
        console.error('Failed to decode base64:', e);
        return 'Error: Could not decode content';
    }
}


function escapeHtml(text) {
    if (!text) return '';
    return text.toString()
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;');
}

function escapeJavaScript(text) {
    if (!text) return '';
    return text.toString()
        .replace(/\\/g, '\\\\')  // Escape backslashes first
        .replace(/`/g, '\\`')    // Escape backticks for template literals
        .replace(/'/g, "\\'")    // Escape single quotes
        .replace(/"/g, '\\"')    // Escape double quotes
        .replace(/\n/g, '\\n')   // Escape newlines
        .replace(/\r/g, '\\r')   // Escape carriage returns
        .replace(/\t/g, '\\t')   // Escape tabs
        .replace(/\$/g, '\\$');  // Escape dollar signs for template literals
}


function showTab(tabName) {
    // Hide all tab contents
    document.querySelectorAll('.tab-content').forEach(tab => {
        tab.classList.add('hidden');
    });
    
    // Remove active classes from all buttons
    document.querySelectorAll('.tab-button').forEach(btn => {
        btn.classList.remove('border-blue-500', 'text-blue-600');
        btn.classList.add('border-transparent', 'text-gray-500');
    });
    
    // Show selected tab
    document.getElementById(tabName).classList.remove('hidden');
    
    // Mark button as active
    const activeButton = document.querySelector(`[data-tab="${tabName}"]`);
    if (activeButton) {
        activeButton.classList.remove('border-transparent', 'text-gray-500');
        activeButton.classList.add('border-blue-500', 'text-blue-600');
    }
}

function formatFileSize(bytes) {
    if (bytes === 0) return '0 Bytes';
    const k = 1024;
    const sizes = ['Bytes', 'KB', 'MB', 'GB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
}

// Chart selector and update functions
async function populateChartSelectors() {
    const callgraphBaseSelect = document.getElementById('callgraph-base-select');
    const callgraphActualSelect = document.getElementById('callgraph-actual-select');
    const flamegraphBaseSelect = document.getElementById('flamegraph-base-select');
    const flamegraphActualSelect = document.getElementById('flamegraph-actual-select');
    
    // Clear existing options (keep first "None" and "Select..." options)
    [callgraphBaseSelect, flamegraphBaseSelect].forEach(select => {
        while (select.children.length > 1) {
            select.removeChild(select.lastChild);
        }
    });
    [callgraphActualSelect, flamegraphActualSelect].forEach(select => {
        while (select.children.length > 1) {
            select.removeChild(select.lastChild);
        }
    });
    
    // Get dumps using centralized data access
    const dumps = await DataAccess.getMemoryDumps();
    
    if (dumps && dumps.length > 0) {
        // Sort dumps by creation time (most recent first)
        const sortedDumps = [...dumps].sort((a, b) => {
            // Use 'created' field (timestamp) or fallback to extracting from filename
            const timeA = a.created || parseInt(a.name.match(/(\d+)/)?.[1]) || 0;
            const timeB = b.created || parseInt(b.name.match(/(\d+)/)?.[1]) || 0;
            return timeB - timeA; // Descending order (most recent first)
        });

        sortedDumps.forEach((dump, index) => {
            const dumpName = dump.name || `dump-${index}`;
            const dumpSize = dump.size || 0;
            // Format Unix timestamp in user's local timezone
            const timestamp = dump.timestamp
                ? new Date(dump.timestamp * 1000).toLocaleString()
                : 'Unknown time';
            const displayText = `${timestamp} (${formatFileSize(dumpSize)})`;

            // Add to base selectors (optional)
            const baseOption1 = document.createElement('option');
            baseOption1.value = dumpName;
            baseOption1.textContent = displayText;
            callgraphBaseSelect.appendChild(baseOption1);
            
            const baseOption2 = document.createElement('option');
            baseOption2.value = dumpName;
            baseOption2.textContent = displayText;
            flamegraphBaseSelect.appendChild(baseOption2);
            
            // Add to actual selectors (required)
            const actualOption1 = document.createElement('option');
            actualOption1.value = dumpName;
            actualOption1.textContent = displayText;
            callgraphActualSelect.appendChild(actualOption1);
            
            const actualOption2 = document.createElement('option');
            actualOption2.value = dumpName;
            actualOption2.textContent = displayText;
            flamegraphActualSelect.appendChild(actualOption2);
        });
        
        console.log(`Populated chart selectors with ${dumps.length} memory dumps`);
    } else {
        console.log('No memory dumps available for chart selectors');
    }
}

// Chart update functions using backtrace processing module
async function updateCallGraph() {
    const baseSelect = document.getElementById('callgraph-base-select');
    const actualSelect = document.getElementById('callgraph-actual-select');
    const container = document.getElementById('callgraph-fullscreen');
    
    const actualDumpName = actualSelect.value;
    
    if (!actualDumpName) {
        container.innerHTML = '<div class="flex items-center justify-center h-full text-gray-500">Select a memory dump to generate call graph...</div>';
        return;
    }
    
    container.innerHTML = '<div class="flex items-center justify-center h-full text-gray-500">Generating call graph...</div>';
    
    // Get the heap dump data using centralized access
    let actualDump;
    try {
        actualDump = await DataAccess.getMemoryDump(actualDumpName);
    } catch (error) {
        container.innerHTML = '<div class="flex items-center justify-center h-full text-red-600">Failed to fetch memory dump</div>';
        return;
    }
    
    try {
        // Parse the heap profile (base64 decoding handled by DataAccess)
        const parser = new BacktraceProcessor.HeapProfileParser();
        const actualProfile = parser.parse(actualDump.data);
        
        let profile = actualProfile;
        
        // Generate call graph data from the profile first
        const actualCallGraphData = BacktraceProcessor.StackProcessor.buildCallGraphData(actualProfile);
        let callGraphData = actualCallGraphData;
        
        // If base dump is selected, compute differential analysis
        const baseDumpName = baseSelect.value;
        if (baseDumpName && baseDumpName !== 'none') {
            // Get base dump using centralized access
            let baseDump;
            try {
                baseDump = await DataAccess.getMemoryDump(baseDumpName);
            } catch (error) {
                console.error('Failed to fetch base dump:', error);
                baseDump = null;
            }
            
            if (baseDump) {
                console.log('Computing differential call graph against base:', baseDumpName);
                
                // Parse base profile and generate its call graph data (base64 decoding handled by DataAccess)
                const baseProfile = parser.parse(baseDump.data);
                const baseCallGraphData = BacktraceProcessor.StackProcessor.buildCallGraphData(baseProfile);
                
                // Compute differential call graph using unified approach
                callGraphData = computeCallGraphDifferential(actualCallGraphData, baseCallGraphData);
                
                // Update title to indicate differential mode
                const titleElement = container.closest('.bg-white')?.querySelector('h2');
                if (titleElement) {
                    titleElement.textContent = `Call Graph Analysis (Differential: ${actualDumpName} - ${baseDumpName})`;
                }
            } else {
                console.warn('Base dump not found:', baseDumpName);
            }
        } else {
            // Reset title for regular mode
            const titleElement = container.closest('.bg-white')?.querySelector('h2');
            if (titleElement) {
                titleElement.textContent = 'Call Graph Analysis';
            }
        }
        
        // Generate call graph SVG using the processed data
        await renderCallGraphWithVizJSData(container, callGraphData);
        
    } catch (error) {
        console.error('Error generating call graph:', error);
        container.innerHTML = '<div class="flex items-center justify-center h-full text-red-600">Error generating call graph. Check console for details.</div>';
    }
}

async function updateFlameGraph() {
    const baseSelect = document.getElementById('flamegraph-base-select');
    const actualSelect = document.getElementById('flamegraph-actual-select');
    const container = document.getElementById('flamegraph-fullscreen');
    
    const actualDumpName = actualSelect.value;
    
    if (!actualDumpName) {
        container.innerHTML = '<div class="flex items-center justify-center h-full text-gray-500">Select a memory dump to generate flame graph...</div>';
        return;
    }
    
    container.innerHTML = '<div class="flex items-center justify-center h-full text-gray-500">Generating flame graph...</div>';
    
    // Get the heap dump data using centralized access
    let actualDump;
    try {
        actualDump = await DataAccess.getMemoryDump(actualDumpName);
    } catch (error) {
        container.innerHTML = '<div class="flex items-center justify-center h-full text-red-600">Failed to fetch memory dump</div>';
        return;
    }
    
    try {
        // Parse the heap profile (base64 decoding handled by DataAccess)
        const parser = new BacktraceProcessor.HeapProfileParser();
        const actualProfile = parser.parse(actualDump.data);
        
        let profile = actualProfile;
        
        // Process stacks and build flame graph data
        const collapsedStacks = BacktraceProcessor.StackProcessor.collapseStacks(profile);
        const flameTree = BacktraceProcessor.StackProcessor.buildFlameTree(collapsedStacks);
        let flameData = BacktraceProcessor.StackProcessor.convertToFlameData(flameTree);
        
        // If base dump is selected, compute differential analysis on flame data
        const baseDumpName = baseSelect.value;
        if (baseDumpName && baseDumpName !== 'none') {
            // Get base dump using centralized access
            let baseDump;
            try {
                baseDump = await DataAccess.getMemoryDump(baseDumpName);
            } catch (error) {
                console.error('Failed to fetch base dump:', error);
                baseDump = null;
            }
            
            if (baseDump) {
                console.log('Computing differential flame graph against base:', baseDumpName);
                
                // Parse base profile and generate flame data (base64 decoding handled by DataAccess)
                const baseProfile = parser.parse(baseDump.data);
                const baseCollapsedStacks = BacktraceProcessor.StackProcessor.collapseStacks(baseProfile);
                const baseFlameTree = BacktraceProcessor.StackProcessor.buildFlameTree(baseCollapsedStacks);
                const baseFlameData = BacktraceProcessor.StackProcessor.convertToFlameData(baseFlameTree);
                
                // Compute differential flame graph
                flameData = computeDifferentialProfile(flameData, baseFlameData);
                
                // Update title to indicate differential mode
                const titleElement = container.closest('.bg-white')?.querySelector('h2');
                if (titleElement) {
                    titleElement.textContent = `Heap Flame Graph Analysis (Differential: ${actualDumpName} - ${baseDumpName})`;
                }
            } else {
                console.warn('Base dump not found:', baseDumpName);
            }
        } else {
            // Reset title for regular mode
            const titleElement = container.closest('.bg-white')?.querySelector('h2');
            if (titleElement) {
                titleElement.textContent = 'Heap Flame Graph Analysis';
            }
        }
        
        // Create chart container safely (renderFlameGraph will handle the sizing)
        container.textContent = ''; // Clear safely
        const chartId = 'flamegraph-chart-' + Date.now();
        const chartDiv = document.createElement('div');
        chartDiv.id = chartId;
        chartDiv.className = 'w-full h-full';
        container.appendChild(chartDiv);
        
        // Initialize ECharts and render flame graph
        setTimeout(() => {
            renderFlameGraph(chartId, flameData);
        }, 100);
        
    } catch (error) {
        console.error('Error generating flame graph:', error);
        container.innerHTML = '<div class="flex items-center justify-center h-full text-red-600">Error generating flame graph. Check console for details.</div>';
    }
}

// Unified differential computation for call graph data (similar to flamegraph approach)
function computeCallGraphDifferential(actualData, baseData) {
    console.log('Computing call graph differential...');
    console.log('Actual call graph nodes:', actualData.nodes.length);
    console.log('Base call graph nodes:', baseData.nodes.length);
    
    // Create maps for quick lookup by node/link identifiers
    const baseNodeMap = new Map();
    const baseLinkMap = new Map();
    const baseReverseLinkMap = new Map();
    
    // Build base data maps
    baseData.nodes.forEach(node => {
        baseNodeMap.set(node.id, node);
    });
    
    baseData.links?.forEach(link => {
        const key = `${link.source}->${link.target}`;
        baseLinkMap.set(key, link);
    });
    
    baseData.reverseLinks?.forEach(link => {
        const key = `${link.source}->${link.target}`;
        baseReverseLinkMap.set(key, link);
    });
    
    console.log('Base nodes mapped:', baseNodeMap.size);
    
    // Process actual data and subtract base data
    const differentialNodes = [];
    actualData.nodes.forEach(actualNode => {
        const baseNode = baseNodeMap.get(actualNode.id);
        const actualMemory = actualNode.memory || 0;
        const baseMemory = baseNode ? (baseNode.memory || 0) : 0;
        
        // Compute differential memory
        const diffMemory = actualMemory - baseMemory;
        
        // Only include nodes with significant memory differences (>1KB) or new nodes
        if (Math.abs(diffMemory) > 1024 || !baseNode) {
            const newNode = {
                ...actualNode,
                memory: diffMemory,
                calls: (actualNode.calls || 0) - (baseNode ? (baseNode.calls || 0) : 0),
                isDifferential: true,
                isNew: !baseNode,
                originalMemory: actualMemory
            };
            differentialNodes.push(newNode);
            
            // Reduce base memory for subsequent matches (similar to flamegraph logic)
            if (baseMemory > 0) {
                baseNodeMap.set(actualNode.id, {
                    ...baseNode,
                    memory: Math.max(0, baseMemory - actualMemory)
                });
            }
        }
    });
    
    // Process links - only include links between nodes that are in differential
    const differentialNodeIds = new Set(differentialNodes.map(n => n.id));
    
    const differentialLinks = [];
    actualData.links?.forEach(actualLink => {
        // Only include links where both endpoints are in the differential
        if (differentialNodeIds.has(actualLink.source) && differentialNodeIds.has(actualLink.target)) {
            const key = `${actualLink.source}->${actualLink.target}`;
            const baseLink = baseLinkMap.get(key);
            const diffValue = (actualLink.value || 0) - (baseLink ? (baseLink.value || 0) : 0);
            
            // Include if significant difference or new link
            if (Math.abs(diffValue) > 1024 || !baseLink) {
                differentialLinks.push({
                    ...actualLink,
                    value: diffValue,
                    isDifferential: true,
                    isNew: !baseLink
                });
            }
        }
    });
    
    const differentialReverseLinks = [];
    actualData.reverseLinks?.forEach(actualLink => {
        // Only include links where both endpoints are in the differential
        if (differentialNodeIds.has(actualLink.source) && differentialNodeIds.has(actualLink.target)) {
            const key = `${actualLink.source}->${actualLink.target}`;
            const baseLink = baseReverseLinkMap.get(key);
            const diffValue = (actualLink.value || 0) - (baseLink ? (baseLink.value || 0) : 0);
            
            // Include if significant difference or new link  
            if (Math.abs(diffValue) > 1024 || !baseLink) {
                differentialReverseLinks.push({
                    ...actualLink,
                    value: diffValue,
                    isDifferential: true,
                    isNew: !baseLink
                });
            }
        }
    });
    
    console.log('Differential call graph data points:', differentialNodes.length);
    console.log('Differential links:', differentialLinks.length);
    console.log('Differential reverse links:', differentialReverseLinks.length);
    
    if (differentialNodes.length === 0) {
        console.log('No significant differential found - returning empty data');
        return { nodes: [], links: [], reverseLinks: [] };
    }
    
    return {
        nodes: differentialNodes,
        links: differentialLinks,
        reverseLinks: differentialReverseLinks
    };
}


// ===== API Integration Functions for Interactive Dashboard =====

// Show loading spinner on button
function showButtonSpinner(button) {
    const textSpan = button.querySelector('.btn-text');
    const spinner = button.querySelector('.btn-spinner');
    if (textSpan && spinner) {
        spinner.classList.remove('hidden');
        button.disabled = true;
    }
}

// Hide loading spinner on button
function hideButtonSpinner(button) {
    const textSpan = button.querySelector('.btn-text');
    const spinner = button.querySelector('.btn-spinner');
    if (textSpan && spinner) {
        spinner.classList.add('hidden');
        button.disabled = false;
    }
}

// Show notification
function showNotification(message, type = 'info') {
    // Create notification element
    const notification = document.createElement('div');
    const bgColor = type === 'error' ? 'bg-red-500' : type === 'success' ? 'bg-green-500' : 'bg-blue-500';
    notification.className = `fixed top-4 right-4 ${bgColor} text-white px-6 py-3 rounded-lg shadow-lg z-50 max-w-md`;
    notification.textContent = message;
    
    document.body.appendChild(notification);
    
    // Remove after 4 seconds
    setTimeout(() => {
        notification.remove();
    }, 4000);
}

// Fetch profiling status
async function updateProfilingStatusFromAPI() {
    try {
        const data = await fetchProfilingStatus();
        updateProfilingStatus(data);
        return data;
    } catch (error) {
        showNotification('Failed to fetch profiling status', 'error');
        return null;
    }
}

// Update profiling status UI
function updateProfilingStatus(status) {
    const indicator = document.getElementById('profiling-status-indicator');
    const text = document.getElementById('profiling-status-text');
    const message = document.getElementById('profiling-status-message');
    const startBtn = document.getElementById('start-profiling-btn');
    const stopBtn = document.getElementById('stop-profiling-btn');
    const dumpBtn = document.getElementById('trigger-dump-btn');

    if (indicator && text && message && startBtn && stopBtn && dumpBtn) {
        const isActive = status.profiling_active;
        const isSupported = status.heap_dumps_available;

        // Update status indicator
        indicator.className = `w-3 h-3 rounded-full ${isActive ? 'bg-green-500' : 'bg-gray-400'}`;
        text.textContent = isActive ? 'Active' : 'Inactive';
        message.textContent = status.message || (isActive ? 'Memory profiling is active' : 'Memory profiling is inactive');

        // Update button states
        if (isSupported) {
            startBtn.disabled = isActive;
            stopBtn.disabled = !isActive;
            dumpBtn.disabled = false;
        } else {
            startBtn.disabled = true;
            stopBtn.disabled = true;
            dumpBtn.disabled = true;
            message.textContent = status.message || 'Memory profiling not supported on this platform';
        }
    }
}

// Start profiling UI handler
async function handleStartProfiling() {
    const button = document.getElementById('start-profiling-btn');
    showButtonSpinner(button);
    
    try {
        await startProfiling();
        showNotification('Memory profiling started successfully', 'success');
        // Refresh status after a short delay
        setTimeout(updateProfilingStatusFromAPI, 1000);
    } catch (error) {
        showNotification(error.message || 'Failed to start profiling', 'error');
    } finally {
        hideButtonSpinner(button);
    }
}

// Stop profiling UI handler
async function handleStopProfiling() {
    const button = document.getElementById('stop-profiling-btn');
    showButtonSpinner(button);
    
    try {
        await stopProfiling();
        showNotification('Memory profiling stopped successfully', 'success');
        // Refresh status after a short delay
        setTimeout(updateProfilingStatusFromAPI, 1000);
    } catch (error) {
        showNotification(error.message || 'Failed to stop profiling', 'error');
    } finally {
        hideButtonSpinner(button);
    }
}

// Trigger dump creation UI handler
async function handleTriggerDump() {
    const button = document.getElementById('trigger-dump-btn');
    showButtonSpinner(button);
    
    try {
        await triggerDump();
        showNotification('Memory dump created successfully', 'success');
        // No need for manual refresh since polling will pick it up automatically
        // Still show a small delay for immediate feedback in case polling misses the rapid change
        setTimeout(refreshDashboardData, 200);
    } catch (error) {
        showNotification(error.message || 'Failed to create dump', 'error');
    } finally {
        hideButtonSpinner(button);
    }
}

// Load and display dumps list
async function refreshDumpsDisplay() {
    console.log('🔄 Starting refreshDumpsDisplay...');
    try {
        console.log('📡 Calling listDumps()...');
        const dumps = await listDumps();
        console.log('✅ listDumps() returned:', dumps);
        console.log('📊 Number of dumps:', dumps ? dumps.length : 'null/undefined');

        await updateDumpsList(dumps);
        console.log('✅ updateDumpsList completed');
        return dumps;
    } catch (error) {
        console.error('❌ Error in refreshDumpsDisplay:', error);
        console.error('❌ Error stack:', error.stack);
        showNotification('Failed to list dumps', 'error');
        return [];
    }
}

// Update dumps list UI
async function updateDumpsList(dumps) {
    console.log('🎨 Starting updateDumpsList with dumps:', dumps);
    const dumpsListElement = document.getElementById('dumps-list');
    console.log('🎯 Found dumps-list element:', !!dumpsListElement);

    if (!dumpsListElement) {
        console.error('❌ dumps-list element not found in DOM');
        return;
    }

    if (!dumps || dumps.length === 0) {
        console.log('📭 No dumps to display, showing empty message');
        const noDumpsDiv = document.createElement('div');
        noDumpsDiv.className = 'text-center text-gray-500';
        noDumpsDiv.textContent = 'No memory dumps available';
        dumpsListElement.innerHTML = '';
        dumpsListElement.appendChild(noDumpsDiv);
        // Clear chart selectors if no dumps
        await populateChartSelectors();
        return;
    }

    console.log(`📋 Displaying ${dumps.length} dumps`);

    // Sort dumps by creation time (most recent first)
    const sortedDumps = [...dumps].sort((a, b) => {
        // Use 'created' field (timestamp) or fallback to extracting from filename
        const timeA = a.created || parseInt(a.name.match(/(\d+)/)?.[1]) || 0;
        const timeB = b.created || parseInt(b.name.match(/(\d+)/)?.[1]) || 0;
        return timeB - timeA; // Descending order (most recent first)
    });

    // Clear dumps list and rebuild using custom elements (XSS-safe)
    dumpsListElement.innerHTML = '';
    console.log('🧹 Cleared dumps list container');

    sortedDumps.forEach((dump, index) => {
        console.log(`🏗️ Creating dump item ${index + 1}:`, dump.name);
        try {
            // Format Unix timestamp in user's local timezone
            const timestampDisplay = dump.timestamp
                ? `Created: ${new Date(dump.timestamp * 1000).toLocaleString()}`
                : '';
            const dumpElement = createDumpItem(
                dump.name,
                `Size: ${formatFileSize(dump.size)}`,
                timestampDisplay
            );
            if (dumpElement) {
                dumpsListElement.appendChild(dumpElement);
                console.log(`✅ Added dump item ${index + 1} to DOM`);
            } else {
                console.error(`❌ Failed to create dump item ${index + 1}: createDumpItem returned null`);
            }
        } catch (error) {
            console.error(`❌ Error creating dump item ${index + 1}:`, error);
        }
    });

    console.log('✅ All dump items added to DOM');

    // Update chart selectors when dumps list changes
    await populateChartSelectors();
    console.log('✅ Chart selectors populated');
}

// Download dump UI handler
async function handleDownloadDump(filename) {
    try {
        const blob = await downloadDump(filename);
        const url = window.URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = filename;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        window.URL.revokeObjectURL(url);
        showNotification(`Downloaded ${filename}`, 'success');
    } catch (error) {
        showNotification(error.message || 'Failed to download dump', 'error');
    }
}

// Delete dump UI handler
async function handleDeleteDump(filename) {
    if (!confirm(`Are you sure you want to delete ${filename}?`)) {
        return;
    }
    
    try {
        await deleteDump(filename);
        showNotification(`Deleted ${filename}`, 'success');
        // Polling will pick up changes automatically, minimal delay for immediate feedback
        setTimeout(refreshDashboardData, 200);
    } catch (error) {
        showNotification(error.message || 'Failed to delete dump', 'error');
    }
}

// Clear all dumps UI handler
async function handleClearAllDumps() {
    const button = document.querySelector('button[onclick="handleClearAllDumps()"]');
    if (!button) return;
    
    // Show confirmation dialog
    const confirmed = confirm('Are you sure you want to clear all heap dump files? This action cannot be undone.');
    if (!confirmed) return;
    
    // Show loading state
    showButtonSpinner(button);
    
    try {
        const result = await clearAllDumps();
        console.log('Clear all dumps result:', result);
        
        // Show success message
        if (result.deleted_count > 0) {
            alert(`Successfully deleted ${result.deleted_count} heap dump files.`);
        } else {
            alert('No heap dump files were found to delete.');
        }
        
        // Polling will pick up changes automatically, minimal delay for immediate feedback
        setTimeout(refreshDashboardData, 200);
    } catch (error) {
        console.error('Error clearing dumps:', error);
        alert(`Error clearing dumps: ${error.message || 'Network error'}`);
    } finally {
        // Hide loading state
        hideButtonSpinner(button);
    }
}

// Polling variables
let dumpPollingInterval = null;
let lastDumpCount = 0;

// Start polling for dump changes (only in dashboard mode)
function startDumpPolling() {
    if (!DataAccess.isDashboardMode()) {
        return; // Don't poll in static mode
    }
    
    // Clear any existing polling
    if (dumpPollingInterval) {
        clearInterval(dumpPollingInterval);
    }
    
    // Poll every 3 seconds
    dumpPollingInterval = setInterval(async () => {
        console.log('⏰ Polling dumps...');
        try {
            const dumps = await DataAccess.getMemoryDumps();
            const currentCount = dumps ? dumps.length : 0;
            console.log(`⏰ Poll result: ${currentCount} dumps (was ${lastDumpCount})`);

            // If dump count changed, refresh the display
            if (currentCount !== lastDumpCount) {
                console.log(`🔄 Dump count changed from ${lastDumpCount} to ${currentCount}, refreshing...`);
                lastDumpCount = currentCount;
                await refreshDumpsDisplay();
            } else {
                console.log('⏰ No change in dump count, skipping refresh');
            }
        } catch (error) {
            console.error('❌ Error during dump polling:', error);
            console.error('❌ Polling error stack:', error.stack);
        }
    }, 3000);
}

// Stop polling
function stopDumpPolling() {
    if (dumpPollingInterval) {
        clearInterval(dumpPollingInterval);
        dumpPollingInterval = null;
    }
}

// Refresh dashboard data
async function refreshDashboardData() {
    await refreshDumpsDisplay();
}


// Initialize Summary tab when dashboard loads
function initializeSummaryTab() {
    // Fetch initial status
    updateProfilingStatusFromAPI();
    
    // Load initial dumps list
    refreshDumpsDisplay();
    
    // Set up periodic status updates (every 5 seconds)
    setInterval(updateProfilingStatusFromAPI, 5000);
}

// Initialize the page
document.addEventListener('DOMContentLoaded', async function() {
    // Load all data and update UI
    await initializeApplicationData();
});

// Clean up polling when page unloads
window.addEventListener('beforeunload', function() {
    stopDumpPolling();
});

// Initialize application data and UI
async function initializeApplicationData() {
    console.log('🚀 Starting application initialization...');
    
    const loadingElements = {
        system: document.getElementById('system-info-content'),
        config: document.getElementById('router-config-content'),
        schema: document.getElementById('schema-content')
    };
    
    console.log('📱 Found UI elements:', {
        system: !!loadingElements.system,
        config: !!loadingElements.config,
        schema: !!loadingElements.schema
    });

    // Set loading states
    Object.values(loadingElements).forEach(el => {
        if (el) el.textContent = 'Loading...';
    });

    try {
        console.log('💾 Checking dashboard mode...');
        console.log('🔍 DataAccess.isDashboardMode():', DataAccess.isDashboardMode());
        
        // Load data using data access layer
        console.log('📡 Starting data load...');
        const data = await loadAllData();
        console.log('✅ Data loaded:', data);
        
        // Update UI with loaded data for both dashboard and static modes
        console.log('🖥️ Updating UI elements with loaded data...');
        
        if (data.systemInfo && loadingElements.system) {
            console.log('📝 Setting system info content (length: ' + data.systemInfo.length + ')');
            loadingElements.system.textContent = data.systemInfo;
        } else {
            console.warn('⚠️ System info not available:', {
                hasData: !!data.systemInfo,
                hasElement: !!loadingElements.system
            });
        }
        
        if (data.routerConfig && loadingElements.config) {
            console.log('⚙️ Setting router config content (length: ' + data.routerConfig.length + ')');
            loadingElements.config.textContent = data.routerConfig;
        } else {
            console.warn('⚠️ Router config not available:', {
                hasData: !!data.routerConfig,
                hasElement: !!loadingElements.config
            });
        }
        
        if (data.schema && loadingElements.schema) {
            console.log('📊 Setting schema content (length: ' + data.schema.length + ')');
            loadingElements.schema.textContent = data.schema;
        } else {
            console.warn('⚠️ Schema not available:', {
                hasData: !!data.schema,
                hasElement: !!loadingElements.schema
            });
        }

        // Handle mode-specific UI adjustments
        if (!DataAccess.isDashboardMode()) {
            // Hide Dashboard tab in static export mode since interactive features won't work
            const dashboardTab = document.getElementById('dashboard-tab');
            if (dashboardTab) {
                dashboardTab.style.display = 'none';
            }
            
            // Show System tab as default instead of Dashboard
            showTab('system');
        }
        
        // Populate chart selectors
        await populateChartSelectors();
        
        // Initialize dashboard-specific features (only in dashboard mode)
        if (DataAccess.isDashboardMode()) {
            // Initialize memory profiling status and periodic updates
            console.log('🔧 Initializing dashboard features...');
            initializeSummaryTab();
            
            // Initialize dump polling
            const dumps = await DataAccess.getMemoryDumps();
            lastDumpCount = dumps.length;
            startDumpPolling();
            console.log('Started dump polling - initial count:', lastDumpCount);
        }
        
    } catch (error) {
        console.error('Failed to initialize application data:', error);
        // Set error messages
        Object.values(loadingElements).forEach(el => {
            if (el) el.textContent = 'Error loading data';
        });
    }
}