// Web Components using customElements.define() - MDN best practice

class LoadingMessage extends HTMLElement {
    constructor() {
        super();
        const template = document.getElementById('loading-message-template').content;
        const shadowRoot = this.attachShadow({ mode: 'open' });
        shadowRoot.appendChild(template.cloneNode(true));
    }
}

class ErrorMessage extends HTMLElement {
    constructor() {
        super();
        const template = document.getElementById('error-message-template').content;
        const shadowRoot = this.attachShadow({ mode: 'open' });
        shadowRoot.appendChild(template.cloneNode(true));
    }
}

class InfoMessage extends HTMLElement {
    constructor() {
        super();
        const template = document.getElementById('info-message-template').content;
        const shadowRoot = this.attachShadow({ mode: 'open' });
        shadowRoot.appendChild(template.cloneNode(true));
    }
}

class CallGraphContainer extends HTMLElement {
    constructor() {
        super();
        const template = document.getElementById('callgraph-container-template').content;
        const shadowRoot = this.attachShadow({ mode: 'open' });
        shadowRoot.appendChild(template.cloneNode(true));
    }

    setSvgContent(svgContent) {
        const slot = this.shadowRoot.querySelector('slot[name="svg-content"]');
        if (slot && svgContent) {
            // Safer approach: parse as DOM first, then insert
            // Note: svgContent comes from viz.js library, not user input
            const parser = new DOMParser();
            const doc = parser.parseFromString(svgContent, 'image/svg+xml');
            const svgElement = doc.documentElement;

            // Check if parsing was successful
            if (svgElement.nodeName === 'svg') {
                // Import the SVG node into our document and replace the slot
                const importedSvg = document.importNode(svgElement, true);
                slot.parentNode.replaceChild(importedSvg, slot);
            } else {
                // Fallback to safe text content if SVG parsing fails
                const errorDiv = document.createElement('div');
                errorDiv.textContent = 'Error: Invalid SVG content';
                errorDiv.className = 'text-red-500 p-4';
                slot.parentNode.replaceChild(errorDiv, slot);
            }
        }
    }
}

class FlameGraphContainer extends HTMLElement {
    constructor() {
        super();
        const template = document.getElementById('flamegraph-container-template').content;
        const shadowRoot = this.attachShadow({ mode: 'open' });
        shadowRoot.appendChild(template.cloneNode(true));
    }

    setResetButtonState(disabled) {
        const resetButton = this.shadowRoot.querySelector('#reset-button');
        if (resetButton) {
            resetButton.disabled = disabled;
        }
    }
}

class DumpItem extends HTMLElement {
    constructor() {
        super();
        const template = document.getElementById('dump-item-template').content;
        const shadowRoot = this.attachShadow({ mode: 'open' });
        shadowRoot.appendChild(template.cloneNode(true));

        // Set up event handlers
        this.shadowRoot.querySelector('.download-btn').addEventListener('click', () => {
            const dumpName = this.getAttribute('dump-name');
            if (dumpName) handleDownloadDump(dumpName);
        });

        this.shadowRoot.querySelector('.delete-btn').addEventListener('click', () => {
            const dumpName = this.getAttribute('dump-name');
            if (dumpName) handleDeleteDump(dumpName);
        });
    }
}

// Define custom elements
customElements.define('loading-message', LoadingMessage);
customElements.define('error-message', ErrorMessage);
customElements.define('info-message', InfoMessage);
customElements.define('callgraph-container', CallGraphContainer);
customElements.define('flamegraph-container', FlameGraphContainer);
customElements.define('dump-item', DumpItem);

// Factory functions for creating custom elements

function createLoadingMessage(message) {
    const element = document.createElement('loading-message');
    element.textContent = message;
    return element;
}

function createErrorMessage(message) {
    const element = document.createElement('error-message');
    element.textContent = message;
    return element;
}

function createInfoMessage(message) {
    const element = document.createElement('info-message');
    element.textContent = message;
    return element;
}

function createCallGraphContainer(title, svgContent) {
    const element = document.createElement('callgraph-container');

    const titleSlot = document.createElement('span');
    titleSlot.slot = 'title';
    titleSlot.textContent = title;
    element.appendChild(titleSlot);

    if (svgContent) {
        element.setSvgContent(svgContent);
    }

    return element;
}

function createFlameGraphContainer(title, resetDisabled = false) {
    const element = document.createElement('flamegraph-container');

    const titleSlot = document.createElement('span');
    titleSlot.slot = 'title';
    titleSlot.textContent = title;
    element.appendChild(titleSlot);

    element.setResetButtonState(resetDisabled);
    return element;
}

function createDumpItem(dumpName, size, timestamp) {
    const element = document.createElement('dump-item');
    element.setAttribute('dump-name', dumpName);

    const nameSlot = document.createElement('span');
    nameSlot.slot = 'name';
    nameSlot.textContent = dumpName;
    element.appendChild(nameSlot);

    const sizeSlot = document.createElement('span');
    sizeSlot.slot = 'size';
    sizeSlot.textContent = size;
    element.appendChild(sizeSlot);

    if (timestamp) {
        const timestampSlot = document.createElement('span');
        timestampSlot.slot = 'timestamp';
        timestampSlot.textContent = timestamp;
        element.appendChild(timestampSlot);
    }

    return element;
}