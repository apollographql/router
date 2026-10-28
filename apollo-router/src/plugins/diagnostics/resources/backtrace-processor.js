/**
 * Apollo Router Diagnostics - Backtrace Processing Module
 * 
 * This module handles parsing and processing of memory profile backtraces,
 * including heap profile (pprof) parsing, stack collapsing, and data structure
 * conversion for visualization.
 */

// Heap Profile Parser

/**
 * Minimal protobuf reader: just what decoding pprof needs.
 */
class ProtoReader {
    constructor(bytes) {
        this.bytes = bytes;
        this.pos = 0;
    }

    done() {
        return this.pos >= this.bytes.length;
    }

    /** Unsigned varint, exact up to 2^53. */
    varint() {
        let result = 0;
        let multiplier = 1;
        let byte;
        do {
            byte = this.bytes[this.pos++];
            result += (byte & 0x7f) * multiplier;
            multiplier *= 128;
        } while (byte & 0x80);
        return result;
    }

    /** A length-delimited field's bytes. */
    lengthDelimited() {
        const length = this.varint();
        const value = this.bytes.subarray(this.pos, this.pos + length);
        this.pos += length;
        return value;
    }

    skip(wireType) {
        switch (wireType) {
            case 0: this.varint(); break;
            case 1: this.pos += 8; break;
            case 2: {
                // Read the length before advancing: `this.pos += this.varint()` would add it
                // to the position from before the length was read.
                const length = this.varint();
                this.pos += length;
                break;
            }
            case 5: this.pos += 4; break;
            default: throw new Error(`Unsupported protobuf wire type ${wireType}`);
        }
    }

    /** Calls `onField(fieldNumber, wireType)` for each field; it must consume or skip the value. */
    forEachField(onField) {
        while (!this.done()) {
            const key = this.varint();
            onField(Math.floor(key / 8), key % 8);
        }
    }

    /** Repeated varints, packed or not. */
    repeatedVarint(wireType, out) {
        if (wireType === 2) {
            const packed = new ProtoReader(this.lengthDelimited());
            while (!packed.done()) {
                out.push(packed.varint());
            }
        } else {
            out.push(this.varint());
        }
    }
}

/**
 * Decode the parts of a pprof `Profile` message the dashboard uses.
 * See https://github.com/google/pprof/blob/main/proto/profile.proto
 */
function decodePprof(bytes) {
    const profile = {sampleTypes: [], samples: [], locations: new Map(), functions: new Map(), strings: []};
    const decoder = new TextDecoder();
    const message = (bytes, onField) => {
        const reader = new ProtoReader(bytes);
        reader.forEachField((field, wireType) => onField(reader, field, wireType));
    };

    message(bytes, (reader, field, wireType) => {
        switch (field) {
            case 1: { // sample_type: ValueType
                const valueType = {type: 0, unit: 0};
                message(reader.lengthDelimited(), (r, f, w) => {
                    if (f === 1) valueType.type = r.varint();
                    else if (f === 2) valueType.unit = r.varint();
                    else r.skip(w);
                });
                profile.sampleTypes.push(valueType);
                break;
            }
            case 2: { // sample: Sample
                const sample = {locationIds: [], values: []};
                message(reader.lengthDelimited(), (r, f, w) => {
                    if (f === 1) r.repeatedVarint(w, sample.locationIds);
                    else if (f === 2) r.repeatedVarint(w, sample.values);
                    else r.skip(w);
                });
                profile.samples.push(sample);
                break;
            }
            case 4: { // location: Location
                const location = {id: 0, address: 0, functionIds: []};
                message(reader.lengthDelimited(), (r, f, w) => {
                    if (f === 1) location.id = r.varint();
                    else if (f === 3) location.address = r.varint();
                    else if (f === 4) { // line: Line
                        message(r.lengthDelimited(), (lr, lf, lw) => {
                            if (lf === 1) location.functionIds.push(lr.varint());
                            else lr.skip(lw);
                        });
                    } else r.skip(w);
                });
                profile.locations.set(location.id, location);
                break;
            }
            case 5: { // function: Function
                const fn = {id: 0, name: 0};
                message(reader.lengthDelimited(), (r, f, w) => {
                    if (f === 1) fn.id = r.varint();
                    else if (f === 2) fn.name = r.varint();
                    else r.skip(w);
                });
                profile.functions.set(fn.id, fn);
                break;
            }
            case 6: // string_table
                profile.strings.push(decoder.decode(reader.lengthDelimited()));
                break;
            default:
                reader.skip(wireType);
        }
    });
    return profile;
}

async function gunzip(bytes) {
    const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream('gzip'));
    return new Uint8Array(await new Response(stream).arrayBuffer());
}

class HeapProfileParser {
    /**
     * Parse a heap dump: gzipped pprof, symbolized by the router.
     *
     * Returns stacks of frame ids (innermost first), the function name for each frame id, and each
     * stack's in-use bytes. A location with inlined functions becomes one frame per function.
     *
     * @param {Uint8Array} profileData - Gzipped (or plain) pprof
     * @returns {Promise<Object>} Parsed profile with symbols, stacks, and memory data
     */
    async parse(profileData) {
        let bytes = profileData;
        if (bytes[0] === 0x1f && bytes[1] === 0x8b) {
            bytes = await gunzip(bytes);
        }
        const pprof = decodePprof(bytes);

        // The in-use bytes value: the first sample type measured in bytes.
        const valueIndex = Math.max(
            0,
            pprof.sampleTypes.findIndex(t => pprof.strings[t.unit] === 'bytes')
        );

        const result = {
            symbols: new Map(),
            stacks: [],
            stackMemory: new Map(),  // Maps stack index to memory usage
        };

        // Frame ids for each location, innermost first. pprof lists a location's inlined
        // functions innermost first, with the caller they were inlined into last.
        const framesByLocation = new Map();
        for (const location of pprof.locations.values()) {
            const frames = location.functionIds.map((functionId, i) => {
                const fn = pprof.functions.get(functionId);
                const frame = `${location.id}.${i}`;
                result.symbols.set(frame, fn ? pprof.strings[fn.name] : `0x${location.address.toString(16)}`);
                return frame;
            });
            if (frames.length === 0) {
                const frame = `${location.id}`;
                result.symbols.set(frame, `0x${location.address.toString(16)}`);
                frames.push(frame);
            }
            framesByLocation.set(location.id, frames);
        }

        pprof.samples.forEach(sample => {
            // Sample locations are innermost first, as the stack processing expects.
            const stack = sample.locationIds.flatMap(id => framesByLocation.get(id) || [`${id}`]);
            result.stacks.push(stack);
            result.stackMemory.set(result.stacks.length - 1, {
                allocations: 0, // pprof heap profiles from jemalloc carry bytes only
                memory: sample.values[valueIndex] || 0,
            });
        });

        console.log(`Parsed ${result.symbols.size} symbols, ${result.stacks.length} stacks`);
        return result;
    }
}

// Stack Processing Utilities
class StackProcessor {
    /**
     * Collapse identical stacks to reduce data size and improve performance
     * @param {Object} profile - Parsed profile data
     * @returns {Map} Map of collapsed stacks with aggregated data
     */
    static collapseStacks(profile) {
        const stackCounts = new Map();
        
        if (profile.stacks && profile.symbols) {
            profile.stacks.forEach((stack, stackIndex) => {
                const stackMemoryData = profile.stackMemory ? profile.stackMemory.get(stackIndex) : null;
                const stackMemory = stackMemoryData ? stackMemoryData.memory : 1;
                const stackAllocations = stackMemoryData ? stackMemoryData.allocations : 1;
                
                // Convert addresses to function names and create stack signature
                const stackNames = stack.map(addr => {
                    const symbol = profile.symbols.get(addr);
                    return this.shortFunctionName(symbol || addr);
                }).reverse(); // Reverse for flame graph (root at bottom)
                
                const stackSignature = stackNames.join(';');
                
                // Collapse identical stacks
                if (stackCounts.has(stackSignature)) {
                    const existing = stackCounts.get(stackSignature);
                    existing.count += 1;
                    existing.memory += stackMemory;
                    existing.allocations += stackAllocations;
                } else {
                    stackCounts.set(stackSignature, {
                        stack: stackNames,
                        count: 1,
                        memory: stackMemory,
                        allocations: stackAllocations
                    });
                }
            });
        } else if (profile.functions) {
            // Handle differential profile - create simple single-level stacks
            profile.functions.forEach(func => {
                const name = this.shortFunctionName(func.name || 'unknown');
                const memory = Math.abs(func.memory || func.allocations || 0);
                
                if (memory > 0) {
                    stackCounts.set(name, {
                        stack: [name],
                        count: 1,
                        memory: memory,
                        allocations: Math.abs(func.allocations || 0)
                    });
                }
            });
        }
        
        console.log(`Stack collapsing: ${profile.stacks?.length || 0} original stacks → ${stackCounts.size} unique stacks`);
        return stackCounts;
    }

    /**
     * Build a flame graph tree from collapsed stacks
     * @param {Map} collapsedStacks - Collapsed stack data
     * @returns {Object} Tree structure for flame graph
     */
    static buildFlameTree(collapsedStacks) {
        const tree = {
            name: 'all',
            value: 0,
            children: new Map()
        };
        
        // Build tree from collapsed stacks (preserving cycles like jeprof)
        for (const [signature, data] of collapsedStacks) {
            let current = tree;
            
            // Add each function in the stack as a level in the tree
            for (let i = 0; i < data.stack.length; i++) {
                const funcName = data.stack[i];
                
                if (!current.children.has(funcName)) {
                    current.children.set(funcName, {
                        name: funcName,
                        value: 0,
                        children: new Map()
                    });
                }
                
                current = current.children.get(funcName);
                current.value += data.memory;
            }
            
            tree.value += data.memory;
        }
        
        console.log(`Flame tree built: total value ${tree.value}, root children: ${tree.children.size}`);
        return tree;
    }

    /**
     * Convert flame tree to ECharts flame graph format
     * @param {Object} tree - Flame tree structure
     * @returns {Array} Array of flame graph rectangles
     */
    static convertToFlameData(tree) {
        const data = [];
        const totalValue = tree.value;
        
        if (totalValue === 0) {
            console.warn('No data available for flame graph - total value is 0');
            return [];
        }
        
        // Recursive function to convert tree to flame graph format
        const processNode = (node, level = 0, start = 0) => {
            const nodeValue = node.value;
            const percentage = (nodeValue / totalValue) * 100;
            
            // Skip very small nodes to improve performance
            if (percentage < 0.01) return start;
            
            // Get color for this function
            const color = ColorUtils.getColorForFunction(node.name);
            
            // Add this node's rectangle
            const flameItem = {
                name: node.name,      // Full function name for tooltip
                value: [
                    level,           // y-axis (stack level)
                    start,           // x-start
                    start + nodeValue, // x-end
                    StackProcessor.shortFunctionName(node.name), // shortened label for display
                    percentage       // percentage for tooltip
                ],
                itemStyle: {
                    color: color
                }
            };
            
            data.push(flameItem);
            
            // Process children
            let childStart = start;
            if (node.children && node.children.size > 0) {
                // Sort children by value (largest first)
                const sortedChildren = Array.from(node.children.values())
                    .sort((a, b) => b.value - a.value);
                
                for (const child of sortedChildren) {
                    childStart = processNode(child, level + 1, childStart);
                }
            }
            
            return start + nodeValue;
        };
        
        // Process all root children
        let currentStart = 0;
        const sortedRootChildren = Array.from(tree.children.values())
            .sort((a, b) => b.value - a.value);
        
        for (const child of sortedRootChildren) {
            currentStart = processNode(child, 0, currentStart);
        }
        
        console.log(`Generated ${data.length} flame graph rectangles`);
        return data;
    }

    /**
     * Build call graph data from profile
     * @param {Object} profile - Parsed profile data
     * @returns {Object} Call graph nodes and links
     */
    static buildCallGraphData(profile) {
        console.log('Building call graph...');
        
        if (!profile.stacks || !profile.symbols) {
            console.warn('No stack or symbol data available');
            return { nodes: [], links: [], reverseLinks: [] };
        }

        // Extract call relationships from stacks
        const callRelations = new Map();
        const reverseCallRelations = new Map();
        const functionStats = new Map();

        profile.stacks.forEach((stack, stackIndex) => {
            const stackMemoryData = profile.stackMemory ? profile.stackMemory.get(stackIndex) : null;
            const stackMemory = stackMemoryData ? stackMemoryData.memory : 1;
            
            // Process each adjacent pair in stack (caller -> callee)
            for (let i = 0; i < stack.length - 1; i++) {
                const callerAddr = stack[i];
                const calleeAddr = stack[i + 1];
                
                const caller = profile.symbols.get(callerAddr) || callerAddr;
                const callee = profile.symbols.get(calleeAddr) || calleeAddr;
                
                // Update function stats
                this.updateFunctionStats(functionStats, caller, stackMemory);
                this.updateFunctionStats(functionStats, callee, stackMemory);
                
                // Track forward call relationship (caller -> callee)
                const relationKey = `${caller}→${callee}`;
                if (!callRelations.has(relationKey)) {
                    callRelations.set(relationKey, { source: caller, target: callee, value: 0 });
                }
                callRelations.get(relationKey).value += stackMemory;
                
                // Track reverse call relationship (callee -> caller)
                const reverseRelationKey = `${callee}→${caller}`;
                if (!reverseCallRelations.has(reverseRelationKey)) {
                    reverseCallRelations.set(reverseRelationKey, { source: callee, target: caller, value: 0 });
                }
                reverseCallRelations.get(reverseRelationKey).value += stackMemory;
            }
        });

        // Convert to graph format (implementation would continue here)
        const nodes = Array.from(functionStats.entries()).map(([name, stats]) => ({
            id: name,                                    // Full name for tooltip
            name: StackProcessor.shortFunctionName(name), // Shortened name for display
            memory: stats.memory,
            calls: stats.calls,
            symbolSize: Math.max(10, Math.min(50, stats.memory / 1000))
        }));

        const links = Array.from(callRelations.values()).map(rel => ({
            source: rel.source,
            target: rel.target,
            value: rel.value
        }));

        const reverseLinks = Array.from(reverseCallRelations.values()).map(rel => ({
            source: rel.source,
            target: rel.target,
            value: rel.value
        }));

        console.log(`Call graph: ${nodes.length} nodes, ${links.length} links, ${reverseLinks.length} reverse links`);
        return { nodes, links, reverseLinks };
    }

    /**
     * Update function statistics
     * @private
     */
    static updateFunctionStats(functionStats, funcName, memory) {
        if (!functionStats.has(funcName)) {
            functionStats.set(funcName, { memory: 0, calls: 0 });
        }
        const stats = functionStats.get(funcName);
        stats.memory += memory;
        stats.calls += 1;
    }

    /**
     * Shorten function names for display
     * @param {string} fullName - Full function name
     * @returns {string} Shortened name
     */
    static shortFunctionName(fullName) {
        // Keep original symbol names without cleanup
        // Only truncate extremely long names for display purposes
        if (fullName.length > 100) {
            return fullName.substring(0, 97) + '...';
        }
        
        return fullName || 'unknown';
    }
}

// Color utilities for visualizations
class ColorUtils {
    static colorTypes = {
        'root': '#8fd3e8',
        'main': '#d95850',
        'tokio': '#eb8146',
        'alloc': '#ffb248',
        'std': '#f2d643',
        'apollo': '#ebdba4',
        'tower': '#fcce10',
        'hyper': '#b5c334',
        'reqwest': '#1bca93',
        'serde': '#9966cc',
        'default': '#95a5a6'
    };

    /**
     * Get color for function based on name patterns
     * @param {string} funcName - Function name
     * @returns {string} Color code
     */
    static getColorForFunction(funcName) {
        // Determine color based on function name prefixes
        const name = funcName.toLowerCase();
        
        for (const [prefix, color] of Object.entries(this.colorTypes)) {
            if (name.includes(prefix)) {
                return color;
            }
        }
        
        // Hash-based color for consistent coloring
        let hash = 0;
        for (let i = 0; i < funcName.length; i++) {
            hash = ((hash << 5) - hash + funcName.charCodeAt(i)) & 0xffffffff;
        }
        
        const colors = ['#e74c3c', '#3498db', '#2ecc71', '#f39c12', '#9b59b6', '#1abc9c', '#34495e'];
        return colors[Math.abs(hash) % colors.length];
    }
}

// Profile processing utilities
class ProfileProcessor {
    /**
     * Compute differential profile between two profiles
     * @param {Object} actualProfile - Current profile
     * @param {Object} baseProfile - Baseline profile
     * @returns {Object} Differential profile
     */
    static computeDifferentialProfile(actualProfile, baseProfile) {
        // Create a differential profile showing the difference between actual and base
        const differential = {
            functions: [],
            locations: actualProfile.locations || [],
            stringTable: actualProfile.stringTable || []
        };
        
        // Build maps for quick lookup
        const baseFunctions = new Map();
        if (baseProfile.functions) {
            baseProfile.functions.forEach(func => {
                const name = func.name || 'unknown';
                baseFunctions.set(name, func);
            });
        }
        
        // Compute differences
        if (actualProfile.functions) {
            actualProfile.functions.forEach(actualFunc => {
                const name = actualFunc.name || 'unknown';
                const baseFunc = baseFunctions.get(name);
                
                const diffFunc = {
                    name: name,
                    allocations: actualFunc.allocations || 0,
                    memory: actualFunc.memory || 0
                };
                
                if (baseFunc) {
                    diffFunc.allocations = (actualFunc.allocations || 0) - (baseFunc.allocations || 0);
                    diffFunc.memory = (actualFunc.memory || 0) - (baseFunc.memory || 0);
                }
                
                // Only include functions with significant differences
                if (Math.abs(diffFunc.allocations) > 0 || Math.abs(diffFunc.memory) > 0) {
                    differential.functions.push(diffFunc);
                }
            });
        }
        
        return differential;
    }
}

// Export classes for use in other modules
if (typeof module !== 'undefined' && module.exports) {
    // Node.js environment
    module.exports = {
        HeapProfileParser,
        StackProcessor,
        ColorUtils,
        ProfileProcessor
    };
} else {
    // Browser environment
    window.BacktraceProcessor = {
        HeapProfileParser,
        StackProcessor,
        ColorUtils,
        ProfileProcessor
    };
}