/**
 * Viz.js Integration for Call Graph Layout
 * 
 * This module integrates viz.js (Graphviz WebAssembly) to generate
 * professional call graph layouts, replacing our manual layer assignment.
 */

class VizJSCallGraphLayout {
    constructor() {
        this.vizInstance = null;
        this.initialized = false;
    }

    /**
     * Initialize viz.js instance (v3 API)
     */
    async initialize() {
        if (this.initialized) return;
        
        try {
            // Check if viz.js v3 is available via viz-global.min.js
            if (typeof window !== 'undefined' && window.Viz) {
                // Viz.js v3 API - get instance
                this.vizInstance = await window.Viz.instance();
                console.log('Viz.js v3 initialized successfully');
                this.initialized = true;
                return;
            }
            
            throw new Error('Viz.js is not available - ensure viz-global.min.js is loaded from jsDelivr');
            
        } catch (error) {
            console.error('Failed to initialize viz.js:', error);
            throw new Error(`Viz.js initialization failed: ${error.message}`);
        }
    }

    /**
     * When using real viz.js, DOT content should be passed as-is to the engine.
     * However, when displaying DOT content in HTML/JavaScript contexts, proper
     * escaping is required. See the HTML template's escapeJavaScript() function.
     */


    /**
     * Escape node ID for DOT format
     * DOT node IDs must be valid identifiers or quoted strings
     */
    escapeNodeId(id) {
        // Remove or replace problematic characters for node IDs
        return id
            .replace(/[{}]/g, '_')           // Replace braces: {{closure}} -> __closure__
            .replace(/[<>]/g, '_')           // Replace angle brackets: <alloc> -> _alloc_
            .replace(/[:]/g, '_')            // Replace colons: std::vec -> std_vec
            .replace(/[^a-zA-Z0-9_]/g, '_')  // Replace any other non-alphanumeric
            .replace(/^(\d)/, '_$1')         // Ensure doesn't start with digit
            .replace(/_+/g, '_')             // Collapse multiple underscores
            .replace(/^_|_$/g, '');          // Remove leading/trailing underscores
    }

    /**
     * Escape label for DOT format
     * Labels can contain more characters but need proper escaping
     */
    escapeLabel(label) {
        return label
            .replace(/\\/g, '\\\\')          // Escape backslashes first
            .replace(/"/g, '\\"')            // Escape quotes
            .replace(/\n/g, '\\n')           // Escape newlines
            .replace(/\r/g, '\\r')           // Escape carriage returns
            .replace(/\t/g, '\\t')           // Escape tabs
            .replace(/\{/g, '\\{')           // Escape left brace
            .replace(/\}/g, '\\}')           // Escape right brace
            .replace(/</g, '\\<')            // Escape less than
            .replace(/>/g, '\\>');           // Escape greater than
    }

    /**
     * Escape text for SVG XML content
     * SVG text needs HTML entity escaping to prevent XML parsing issues
     */
    escapeForSVG(text) {
        if (!text) return '';
        return text.toString()
            .replace(/&/g, '&amp;')    // Escape ampersands first
            .replace(/</g, '&lt;')     // Escape less than
            .replace(/>/g, '&gt;')     // Escape greater than
            .replace(/"/g, '&quot;')   // Escape quotes
            .replace(/'/g, '&#39;');   // Escape single quotes
    }



    /**
     * Generate SVG directly from DOT graph for verification
     * @param {Array} nodes - Array of nodes
     * @param {Array} links - Array of links
     * @param {Object} options - Layout options
     * @returns {Promise<string>} SVG string
     */
    async generateSVG(nodes, links, options = {}) {
        await this.initialize();
        
        const {
            engine = 'dot',
            width = 1200,
            height = 800
        } = options;

        try {
            if (!this.vizInstance) {
                throw new Error('Viz.js not initialized');
            }
            
            // Build DOT graph with SVG-specific styling
            const dotGraph = this.buildDOTGraphForSVG(nodes, links, { ...options, width, height });

            // Use real viz.js v3 API to render SVG
            const svgResult = this.vizInstance.renderString(dotGraph, {
                engine: engine,
                format: 'svg'
            });
            
            // Post-process SVG to use percentage-based dimensions instead of explicit pixels
            const svgWithPercentageDimensions = this.convertSVGToPercentageDimensions(svgResult);
            
            return svgWithPercentageDimensions;

        } catch (error) {
            console.error('Failed to generate SVG with Viz.js:', error);
            throw new Error(`SVG generation failed: ${error.message}`);
        }
    }

    /**
     * Build DOT graph optimized for SVG output
     * @param {Array} nodes - Array of nodes
     * @param {Array} links - Array of links  
     * @param {Object} options - Options
     * @returns {string} DOT format graph
     */
    buildDOTGraphForSVG(nodes, links, options = {}) {
        const {
            rankdir = 'TB',
            width = 1200,
            height = 800,
            dpi = 96
        } = options;

        let dot = 'digraph callgraph {\n';
        
        // Graph attributes optimized for SVG
        dot += `  size="${width/dpi},${height/dpi}";\n`;
        dot += `  dpi=${dpi};\n`;
        dot += `  rankdir="${rankdir}";\n`;
        dot += '  concentrate=true;\n';
        dot += '  splines="true";\n';  // Use true instead of ortho to avoid edge label issues
        dot += '  overlap=false;\n';
        dot += '  fontname="Arial";\n';
        dot += '  fontsize=12;\n';
        dot += '  bgcolor="white";\n';
        dot += '  pad=0.5;\n';
        dot += '\n';
        
        // Node defaults
        dot += '  node [\n';
        dot += '    shape="box",\n';
        dot += '    style="rounded,filled",\n';
        dot += '    fontname="Arial",\n';
        dot += '    fontsize=10,\n';
        dot += '    margin="0.1,0.05",\n';
        dot += '    penwidth=2\n';
        dot += '  ];\n';
        dot += '\n';
        
        // Edge defaults  
        dot += '  edge [\n';
        dot += '    fontname="Arial",\n';
        dot += '    fontsize=8,\n';
        dot += '    arrowsize=0.8,\n';
        dot += '    penwidth=1.5\n';
        dot += '  ];\n';
        dot += '\n';

        // Add nodes with enhanced styling for SVG
        nodes.forEach(node => {
            const nodeId = this.escapeNodeId(node.id || node.name);
            const label = this.escapeLabel(node.name || node.id);
            const memoryMB = node.memory ? (node.memory / 1024 / 1024).toFixed(1) : '0.0';
            
            // Enhanced color scheme
            const { color, fillcolor } = this.getSVGNodeColors(node.memory || 0);
            
            dot += `  "${nodeId}" [\n`;
            dot += '    label="' + label + '\\n' + memoryMB + ' MB",\n';
            dot += `    color="${color}",\n`;
            dot += `    fillcolor="${fillcolor}",\n`;
            dot += `    fontcolor="white"\n`;
            dot += '  ];\n';
        });
        
        dot += '\n';

        // Add edges with memory-based styling
        links.forEach(link => {
            const sourceId = this.escapeNodeId(link.source);
            const targetId = this.escapeNodeId(link.target);
            const weight = link.value || 1;
            const memoryMB = weight ? (weight / 1024 / 1024).toFixed(1) : '';
            
            // Enhanced edge styling
            const penwidth = Math.max(1, Math.min(6, weight / (1024 * 1024)));
            const color = this.getEdgeColor(weight);
            
            dot += `  "${sourceId}" -> "${targetId}" [\n`;
            dot += `    penwidth=${penwidth.toFixed(1)},\n`;
            dot += `    color="${color}",\n`;
            if (memoryMB && parseFloat(memoryMB) > 0.1) {
                dot += `    label="${memoryMB}MB",\n`;
                dot += '    labelfontsize=8,\n';
            }
            dot += '  ];\n';
        });

        dot += '}';
        return dot;
    }

    /**
     * Get enhanced node colors for SVG
     */
    getSVGNodeColors(memory) {
        const memoryMB = memory / 1024 / 1024;
        if (memoryMB > 10) {
            return { color: '#c62828', fillcolor: '#d32f2f' };  // Dark red
        }
        if (memoryMB > 1) {
            return { color: '#ef6c00', fillcolor: '#f57c00' };  // Dark orange
        }
        if (memoryMB > 0.1) {
            return { color: '#1565c0', fillcolor: '#1976d2' };  // Dark blue
        }
        return { color: '#424242', fillcolor: '#757575' };      // Gray
    }

    /**
     * Get edge color based on memory transfer
     */
    getEdgeColor(memory) {
        const memoryMB = memory / 1024 / 1024;
        if (memoryMB > 5) return '#d32f2f';   // Red for high memory transfer
        if (memoryMB > 1) return '#f57c00';   // Orange for medium
        if (memoryMB > 0.1) return '#1976d2'; // Blue for low
        return '#757575';                     // Gray for very low
    }




    
    /**
     * Convert SVG explicit pixel dimensions to percentage dimensions for responsive layout
     * Only modifies the root <svg> element, not internal elements
     * @param {string} svgContent - Original SVG content with pixel dimensions
     * @returns {string} SVG content with percentage dimensions
     */
    convertSVGToPercentageDimensions(svgContent) {
        try {
            // Extract original dimensions from the root SVG element for viewBox
            const svgTagMatch = svgContent.match(/<svg([^>]*)>/);
            if (!svgTagMatch) {
                console.warn('No SVG tag found in content');
                return svgContent;
            }
            
            const svgAttributes = svgTagMatch[1];
            const widthMatch = svgAttributes.match(/width="(\d+(?:\.\d+)?)(?:pt|px)?"/);
            const heightMatch = svgAttributes.match(/height="(\d+(?:\.\d+)?)(?:pt|px)?"/);
            
            if (!widthMatch || !heightMatch) {
                console.warn('Could not find width/height attributes in SVG tag');
                return svgContent;
            }
            
            const originalWidth = parseFloat(widthMatch[1]);
            const originalHeight = parseFloat(heightMatch[1]);
            
            // Replace the entire SVG opening tag with percentage dimensions and viewBox
            const hasViewBox = svgAttributes.includes('viewBox=');
            let newSvgTag;
            
            if (hasViewBox) {
                // Keep existing viewBox, just replace width/height with percentages
                newSvgTag = svgTagMatch[0]
                    .replace(/width="\d+(?:\.\d+)?(?:pt|px)?"/, 'width="100%"')
                    .replace(/height="\d+(?:\.\d+)?(?:pt|px)?"/, 'height="100%"');
                console.log('SVG already has viewBox, converted to percentage dimensions');
            } else {
                // Add viewBox and set percentage dimensions
                const otherAttributes = svgAttributes
                    .replace(/width="\d+(?:\.\d+)?(?:pt|px)?"/, '')
                    .replace(/height="\d+(?:\.\d+)?(?:pt|px)?"/, '')
                    .trim();
                
                newSvgTag = `<svg width="100%" height="100%" viewBox="0 0 ${originalWidth} ${originalHeight}"${otherAttributes ? ' ' + otherAttributes : ''}>`;
                console.log(`Added viewBox="0 0 ${originalWidth} ${originalHeight}" to SVG for responsive scaling`);
            }
            
            // Replace only the first SVG tag (root element)
            const modifiedSVG = svgContent.replace(/<svg([^>]*)>/, newSvgTag);
            
            return modifiedSVG;
            
        } catch (error) {
            console.error('Failed to convert SVG dimensions:', error);
            // Return original SVG if conversion fails
            return svgContent;
        }
    }
}

// Export for use in other modules
if (typeof module !== 'undefined' && module.exports) {
    module.exports = VizJSCallGraphLayout;
} else {
    // Make available globally in browser
    window.VizJSCallGraphLayout = VizJSCallGraphLayout;
}

// In browser environments, also make it available for immediate use
if (typeof window !== 'undefined') {
    window.VizJSCallGraphLayout = VizJSCallGraphLayout;
}