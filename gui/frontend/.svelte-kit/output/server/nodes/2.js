

export const index = 2;
let component_cache;
export const component = async () => component_cache ??= (await import('../entries/pages/_page.svelte.js')).default;
export const imports = ["_app/immutable/nodes/2.B6muBM1K.js","_app/immutable/chunks/BOkfkSpx.js","_app/immutable/chunks/bvTm1Fpp.js"];
export const stylesheets = [];
export const fonts = [];
