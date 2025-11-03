/**
 * Flame Graph Rendering Module for Apollo Router Diagnostics
 *
 * Renders interactive flame graphs using Apache ECharts for visualizing
 * memory allocation patterns from heap dumps. Supports zooming, filtering,
 * differential analysis, and image export.
 *
 * ## Features
 *
 * - **Interactive Visualization**: Click to zoom, hover for details
 * - **Differential Mode**: Compare two heap dumps to show allocation changes
 * - **Function Filtering**: Focus on specific functions and their call trees
 * - **Image Export**: Save flame graphs as PNG for documentation
 * - **Color Coding**: Visual distinction for positive/negative memory changes
 *
 * ## Data Format
 *
 * Expects flame data in collapsed stack format:
 * ```javascript
 * [
 *   { name: "function_name", value: [level, startPos, size, parentId] },
 *   ...
 * ]
 * ```
 *
 * ## Integration
 *
 * Called by main.js after backtrace-processor.js transforms heap dumps
 * into flame-compatible format.
 *
 * @module flamegraph-renderer
 */

function renderFlameGraph(containerId, flameData) {
    const container = document.getElementById(containerId);
    if (!container) return;

    if (!flameData || flameData.length === 0) {
        container.innerHTML = '<div class="flex items-center justify-center h-full text-gray-500">No flame graph data available</div>';
        return;
    }
    
    const maxLevel = Math.max(...flameData.map(d => d.value[0]));
    const maxValue = Math.max(...flameData.map(d => d.value[2]));
    

    
    // Create safe DOM structure without innerHTML for security
    const focusedInfo = window.currentFocusedFunction ? ` - Focused: ${window.currentFocusedFunction}` : '';
    const resetDisabled = !window.currentFocusedFunction;

    container.textContent = ''; // Clear safely

    // Create main container
    const mainDiv = document.createElement('div');
    mainDiv.className = 'h-full flex flex-col';

    // Create header with controls
    const header = document.createElement('div');
    header.className = 'bg-gray-100 p-2 border-b flex justify-between items-center';

    // Create title
    const title = document.createElement('div');
    title.className = 'text-sm font-semibold text-gray-700';
    title.textContent = `Flame Graph - ${flameData.length} functions${focusedInfo}`;

    // Create controls container
    const controlsContainer = document.createElement('div');
    controlsContainer.className = 'flex gap-2';

    // Create control buttons safely
    const resetButton = document.createElement('button');
    resetButton.className = 'px-3 py-1 bg-gray-500 text-white text-xs rounded hover:bg-gray-600 disabled:opacity-50 disabled:cursor-not-allowed';
    resetButton.textContent = 'Reset View';
    resetButton.disabled = resetDisabled;
    resetButton.setAttribute('onclick', 'resetFlameGraphView()');

    const saveButton = document.createElement('button');
    saveButton.className = 'px-3 py-1 bg-blue-500 text-white text-xs rounded hover:bg-blue-600';
    saveButton.textContent = 'Save as Image';
    saveButton.setAttribute('onclick', 'saveFlameGraph()');

    controlsContainer.appendChild(resetButton);
    controlsContainer.appendChild(saveButton);

    header.appendChild(title);
    header.appendChild(controlsContainer);

    // Create chart wrapper
    const chartWrapper = document.createElement('div');
    chartWrapper.className = 'flex-1 bg-white relative';
    chartWrapper.id = 'flamegraph-chart-wrapper';

    mainDiv.appendChild(header);
    mainDiv.appendChild(chartWrapper);
    container.appendChild(mainDiv);

    // chartWrapper is already defined above, no need to query again

    // Get container height BEFORE clearing content to avoid measurement issues
    const wrapperHeight = chartWrapper.offsetHeight || chartWrapper.clientHeight || 600;
    console.log('Initial wrapper height:', chartWrapper);

    // Clear content and set fixed height immediately
    chartWrapper.innerHTML = '';
    chartWrapper.style.height = wrapperHeight + 'px';
    chartWrapper.style.overflowY = 'scroll'; // Always show vertical scrollbar
    chartWrapper.style.overflowX = 'hidden'; // Never show horizontal scrollbar
    chartWrapper.className = "bg-white relative"; // Remove the flex class to avoid layout issues


    // Calculate chart dimensions
    const minBarHeight = 24;
    const minRequiredHeight = (maxLevel + 1) * minBarHeight + 20; // Minimal padding to reduce empty space
    const chartHeight = minRequiredHeight;
    
    console.log('Required height:', minRequiredHeight, 'Container height:', wrapperHeight);
    
    // Create a wrapper div that provides the scrollable space
    const wrapperDiv = document.createElement('div');
    wrapperDiv.style.width = '100%';
    wrapperDiv.style.height = Math.max(wrapperHeight, chartHeight) + 'px';
    wrapperDiv.style.position = 'relative';
    console.log('Wrapper height:', Math.max(wrapperHeight, chartHeight), 'Container height:', wrapperHeight, 'Chart height:', chartHeight);

    // Create chart div anchored to bottom of wrapper
    const chartDiv = document.createElement('div');
    chartDiv.style.width = '100%';
    chartDiv.style.height = chartHeight + 'px';
    chartDiv.style.position = 'absolute';
    chartDiv.style.bottom = '0';
    chartDiv.style.left = '0';
    chartDiv.style.right = '0';
    
    wrapperDiv.appendChild(chartDiv);
    chartWrapper.appendChild(wrapperDiv);
    
    const chart = echarts.init(chartDiv);
    
    const option = {
        tooltip: {
            trigger: 'item'
        },
        grid: {
            top: 5,
            bottom: 5,
            left: 5,
            right: 5,
            containLabel: false
        },
        xAxis: {
            show: false,
            min: 0,
            max: maxValue
        },
        yAxis: {
            show: false,
            min: -0.5,
            max: maxLevel + 0.5,
            inverse: false
        },
        series: [{
            type: 'custom',
            renderItem: renderFlameItem,
            encode: {
                x: [1, 2],
                y: 0
            },
            data: flameData,
            tooltip: {
                formatter: (params) => {
                    const samples = params.value[2] - params.value[1];
                    const percentage = params.value[4];
                    const memoryFormatted = samples > 1024*1024 ? 
                        `${(samples / (1024*1024)).toFixed(2)} MB` : 
                        samples > 1024 ? 
                        `${(samples / 1024).toFixed(2)} KB` : 
                        `${samples} bytes`;
                    
                    // Get full function name and escape HTML characters (especially < and >)
                    const rawFunctionName = params.data?.name || params.name || params.value[3] || 'Unknown';
                    const fullFunctionName = escapeHtml(rawFunctionName);
                    
                    return `<b>${fullFunctionName}</b><br/>
                           Memory: ${memoryFormatted}<br/>
                           Percentage: ${percentage.toFixed(2)}%`;
                }
            }
        }]
    };

    chart.setOption(option);
    
    // Scroll to bottom to show root functions initially
    console.log('Chart wrapper scroll props:', {
        scrollHeight: chartWrapper.scrollHeight,
        clientHeight: chartWrapper.clientHeight,
        offsetHeight: chartWrapper.offsetHeight,
        scrollTop: chartWrapper.scrollTop
    });
    
    // Scroll the chart wrapper to bottom (this is now the scrolling container)
    chartWrapper.scrollTop = chartWrapper.scrollHeight - chartWrapper.clientHeight;
    console.log('After scrollTop attempt:', chartWrapper.scrollTop);
    
    // Add click event for drill-down functionality
    chart.on('click', (params) => {
        if (params.data && params.data.name) {
            console.log('Focusing on:', params.data.name);
            focusFlameGraphFunction(params.data.name, flameData, chart);
        }
    });
    
    // Store references for later use
    window.currentFlameChart = chart;
    // Only store original data on first render, not on focus/reset renders
    if (!window.originalFlameData) {
        window.originalFlameData = flameData;
    }
    window.currentFlameData = flameData;
    
    // Handle window resize
    const resizeHandler = () => chart.resize();
    window.addEventListener('resize', resizeHandler);
    
    // Cleanup function
    container._chartCleanup = () => {
        window.removeEventListener('resize', resizeHandler);
        chart.dispose();
        // Clear global references
        if (window.currentFlameChart === chart) {
            window.currentFlameChart = null;
            window.currentFlameData = null;
        }
    };
}

// Custom render function for flame graph rectangles
function renderFlameItem(params, api) {
    const level = api.value(0);
    const start = api.coord([api.value(1), level]);
    const end = api.coord([api.value(2), level]);
    const defaultHeight = ((api.size && api.size([0, 1])) || [0, 20])[1];
    const width = end[0] - start[0];

    // Set minimum height for flame rectangles (24px minimum for better visibility)
    const minHeight = 24;
    const height = Math.max(minHeight, defaultHeight);

    // Set minimum width for flame rectangles (10px minimum)
    const minWidth = 10;

    // Don't render very thin rectangles that would be less than minimum
    if (width < 0.5) return null;

    // Apply minimum width
    const displayWidth = Math.max(minWidth, width);

    return {
        type: 'rect',
        transition: ['shape'],
        shape: {
            x: start[0],
            y: start[1] - height / 2,
            width: displayWidth,
            height: height - 2, // itemGap
            r: 1
        },
        style: {
            fill: api.visual('color'),
            stroke: '#fff',
            lineWidth: 0.5
        },
        textConfig: {
            position: 'insideLeft'
        },
        textContent: {
            style: {
                text: displayWidth > 50 ? api.value(3) : '', // Only show text if wide enough
                fontFamily: 'Arial',
                fontSize: Math.min(12, Math.max(8, displayWidth / 10)),
                fill: '#000',
                width: displayWidth - 4,
                overflow: 'truncate'
            }
        }
    };
}

// Flamegraph interaction functions
function focusFlameGraphFunction(targetName, originalData, chart) {
    console.log('Focusing on function:', targetName);
    
    // Filter data to show only the selected function and its children
    const filteredData = filterFlameData(originalData, targetName);
    
    if (filteredData.length > 0) {
        // Re-render with filtered data using the same logic as initial render
        renderFlameGraph('flamegraph-fullscreen', filteredData);
        
        // Focus info is now shown in the embedded chart header
        
        // Store filtered state
        window.currentFilteredData = filteredData;
        window.currentFocusedFunction = targetName;
    }
}

function filterFlameData(originalData, targetName) {
    const filteredData = [];
    let found = false;
    let targetLevel = -1;
    let targetStart = 0;
    let targetWidth = 0;
    
    // Find the target function
    for (const item of originalData) {
        if (item.name === targetName && !found) {
            found = true;
            targetLevel = item.value[0];
            targetStart = item.value[1];
            targetWidth = item.value[2] - item.value[1];
            
            // Adjust the target item to start at 0
            const adjustedItem = { ...item };
            adjustedItem.value = [0, 0, targetWidth, item.name, item.value[4]];
            filteredData.push(adjustedItem);
        } else if (found && item.value[0] > targetLevel) {
            // This is potentially a child of the target function
            const itemStart = item.value[1];
            const itemEnd = item.value[2];
            
            // Check if this item is within the target function's boundaries
            if (itemStart >= targetStart && itemEnd <= (targetStart + targetWidth)) {
                const adjustedItem = { ...item };
                adjustedItem.value = [
                    item.value[0] - targetLevel, // Adjust level
                    item.value[1] - targetStart, // Adjust start position
                    item.value[2] - targetStart, // Adjust end position
                    item.name,
                    item.value[4]
                ];
                filteredData.push(adjustedItem);
            }
        } else if (found && item.value[0] <= targetLevel) {
            // We've moved to a sibling or back up the stack, but continue looking
            // in case there are multiple instances of the same function
            continue;
        }
    }
    
    return filteredData.length > 0 ? filteredData : originalData;
}

function resetFlameGraphView() {
    if (window.currentFlameChart && window.originalFlameData) {
        console.log('Resetting flame graph view...');
        
        // Re-render the entire chart with original unfiltered data
        renderFlameGraph('flamegraph-fullscreen', window.originalFlameData);
        
        // Focus info is now managed within the embedded chart header
        
        // Clear filtered state
        window.currentFilteredData = null;
        window.currentFocusedFunction = null;
        
        console.log('Flame graph view reset to original');
    }
}

function saveFlameGraph() {
    if (window.currentFlameChart) {
        try {
            // Generate image URL
            const imageDataURL = window.currentFlameChart.getDataURL({
                type: 'png',
                pixelRatio: 2,
                backgroundColor: '#ffffff'
            });
            
            // Create download link
            const link = document.createElement('a');
            const timestamp = new Date().toISOString().replace(/[:.]/g, '-');
            const focusedName = window.currentFocusedFunction || 'full';
            const filename = `flamegraph-${focusedName}-${timestamp}.png`;
            
            link.download = filename;
            link.href = imageDataURL;
            document.body.appendChild(link);
            link.click();
            document.body.removeChild(link);
            
            console.log('Flame graph saved as:', filename);
        } catch (error) {
            console.error('Error saving flame graph:', error);
            alert('Error saving flame graph. Please try again.');
        }
    }
}

// Differential profile computation for flame graph data
function computeDifferentialProfile(actualData, baseData) {
    if (!baseData || baseData.length === 0) {
        return actualData; // No base profile, return actual as-is
    }
    
    console.log('Computing differential profile...');
    console.log('Actual data points:', actualData.length);
    console.log('Base data points:', baseData.length);
    
    // Create maps for quick lookup by function name
    const baseMemoryMap = new Map();
    
    // Build base profile memory map using function names
    baseData.forEach(item => {
        const functionName = item.name || item.value[3] || 'unknown';
        const memory = item.value[2] - item.value[1]; // memory size
        
        // Accumulate memory for functions that appear multiple times
        if (baseMemoryMap.has(functionName)) {
            baseMemoryMap.set(functionName, baseMemoryMap.get(functionName) + memory);
        } else {
            baseMemoryMap.set(functionName, memory);
        }
    });
    
    console.log('Base functions mapped:', baseMemoryMap.size);
    
    // Process actual data and subtract base memory
    const differentialData = [];
    
    actualData.forEach(item => {
        const functionName = item.name || item.value[3] || 'unknown';
        const actualMemory = item.value[2] - item.value[1];
        const baseMemory = baseMemoryMap.get(functionName) || 0;
        
        // Subtract base memory, clamp to zero
        const diffMemory = Math.max(0, actualMemory - baseMemory);
        
        // Only include items with positive differential memory
        if (diffMemory > 0) {
            const newItem = {
                ...item,
                value: [
                    item.value[0], // level
                    item.value[1], // start position (will be recomputed)
                    item.value[1] + diffMemory, // end position (adjusted for diff memory)
                    item.value[3], // function name
                    0 // percentage will be recomputed after rebalancing
                ]
            };
            differentialData.push(newItem);
            
            // Reduce the base memory available for this function for subsequent matches
            if (baseMemory > 0) {
                baseMemoryMap.set(functionName, Math.max(0, baseMemory - actualMemory));
            }
        }
    });
    
    console.log('Differential data points:', differentialData.length);
    
    if (differentialData.length === 0) {
        console.log('No positive differential found - returning empty array');
        return [];
    }
    
    // Rebalance the flame graph to make it coherent after subtraction
    return rebalanceFlameGraph(differentialData);
}

// Rebalance flame graph after differential computation
function rebalanceFlameGraph(data) {
    if (data.length === 0) return data;
    
    // Sort by level and position
    data.sort((a, b) => {
        if (a.value[0] !== b.value[0]) return a.value[0] - b.value[0]; // level first
        return a.value[1] - b.value[1]; // then position
    });
    
    // Recompute positions to maintain flame graph structure
    const levels = new Map();
    
    data.forEach(item => {
        const level = item.value[0];
        if (!levels.has(level)) {
            levels.set(level, []);
        }
        levels.get(level).push(item);
    });
    
    // Rebalance each level
    let currentPosition = 0;
    levels.forEach((levelItems, level) => {
        if (level === 0) {
            // Root level - pack items sequentially
            currentPosition = 0;
            levelItems.forEach(item => {
                const memory = item.value[2] - item.value[1];
                item.value[1] = currentPosition;
                item.value[2] = currentPosition + memory;
                currentPosition += memory;
            });
        } else {
            // Higher levels - maintain relative positioning but compact
            let levelPosition = 0;
            levelItems.forEach(item => {
                const memory = item.value[2] - item.value[1];
                item.value[1] = levelPosition;
                item.value[2] = levelPosition + memory;
                levelPosition += memory;
            });
        }
    });
    
    return data;
}