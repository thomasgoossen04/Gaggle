export const manifest = (() => {
function __memo(fn) {
	let value;
	return () => value ??= (value = fn());
}

return {
	appDir: "_app",
	appPath: "_app",
	assets: new Set(["favicon.png","Inter-Medium.ttf","style.css","svelte.svg","wails.png"]),
	mimeTypes: {".png":"image/png",".ttf":"font/ttf",".css":"text/css",".svg":"image/svg+xml"},
	_: {
		client: {start:"_app/immutable/entry/start.CcyJ9ooO.js",app:"_app/immutable/entry/app.Be2BShpF.js",imports:["_app/immutable/entry/start.CcyJ9ooO.js","_app/immutable/chunks/DH-iivNG.js","_app/immutable/chunks/BOkfkSpx.js","_app/immutable/entry/app.Be2BShpF.js","_app/immutable/chunks/BOkfkSpx.js","_app/immutable/chunks/bvTm1Fpp.js"],stylesheets:[],fonts:[],uses_env_dynamic_public:false},
		nodes: [
			__memo(() => import('./nodes/0.js')),
			__memo(() => import('./nodes/1.js')),
			__memo(() => import('./nodes/2.js'))
		],
		remotes: {
			
		},
		routes: [
			{
				id: "/",
				pattern: /^\/$/,
				params: [],
				page: { layouts: [0,], errors: [1,], leaf: 2 },
				endpoint: null
			}
		],
		prerendered_routes: new Set([]),
		matchers: async () => {
			
			return {  };
		},
		server_assets: {}
	}
}
})();
