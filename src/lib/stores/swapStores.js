import { writable } from 'svelte/store';

// Array of active or completed atomic swaps
export const activeSwaps = writable([]);

// Current state of the swap wizard
export const swapWizardState = writable({
    isOpen: false,
    step: 1,
    mode: 'create', // 'create' or 'accept'
    offerChain: 'hemp',
    offerAmount: 0,
    offerAsset: '',
    offerAddress: '',
    wantChain: 'rvn',
    wantAmount: 0,
    wantAsset: '',
    wantAddress: '',
    generatedCode: null,
    acceptedCode: null,
    activeSwapId: null,
    error: null
});
