
// this file is generated — do not edit it


/// <reference types="@sveltejs/kit" />

/**
 * Environment variables [loaded by Vite](https://vitejs.dev/guide/env-and-mode.html#env-files) from `.env` files and `process.env`. Like [`$env/dynamic/private`](https://svelte.dev/docs/kit/$env-dynamic-private), this module cannot be imported into client-side code. This module only includes variables that _do not_ begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) _and do_ start with [`config.kit.env.privatePrefix`](https://svelte.dev/docs/kit/configuration#env) (if configured).
 * 
 * _Unlike_ [`$env/dynamic/private`](https://svelte.dev/docs/kit/$env-dynamic-private), the values exported from this module are statically injected into your bundle at build time, enabling optimisations like dead code elimination.
 * 
 * ```ts
 * import { API_KEY } from '$env/static/private';
 * ```
 * 
 * Note that all environment variables referenced in your code should be declared (for example in an `.env` file), even if they don't have a value until the app is deployed:
 * 
 * ```
 * MY_FEATURE_FLAG=""
 * ```
 * 
 * You can override `.env` values from the command line like so:
 * 
 * ```sh
 * MY_FEATURE_FLAG="enabled" npm run dev
 * ```
 */
declare module '$env/static/private' {
	export const ALLUSERSPROFILE: string;
	export const AMDRMPATH: string;
	export const ANDROID_HOME: string;
	export const APPDATA: string;
	export const BUN_INSPECT_CONNECT_TO: string;
	export const CABAL_DIR: string;
	export const CHOCOLATEYINSTALL: string;
	export const CHOCOLATEYLASTPATHUPDATE: string;
	export const CHROME_CRASHPAD_PIPE_NAME: string;
	export const COLOR: string;
	export const COLORTERM: string;
	export const COMMONPROGRAMFILES: string;
	export const COMMONPROGRAMW6432: string;
	export const COMPUTERNAME: string;
	export const COMSPEC: string;
	export const CONDA_PROMPT_MODIFIER: string;
	export const DRIVERDATA: string;
	export const EDITOR: string;
	export const FRONTEND_DEVSERVER_URL: string;
	export const GHCUP_INSTALL_BASE_PREFIX: string;
	export const GHCUP_MSYS2_ENV: string;
	export const GIT_ASKPASS: string;
	export const GOPATH: string;
	export const HAXEPATH: string;
	export const HOME: string;
	export const HOMEDRIVE: string;
	export const HOMEPATH: string;
	export const INIT_CWD: string;
	export const JAVA_HOME: string;
	export const LANG: string;
	export const LOCALAPPDATA: string;
	export const LOGONSERVER: string;
	export const NEKO_INSTPATH: string;
	export const NODE: string;
	export const NODE_ENV: string;
	export const NODE_EXE: string;
	export const NPM_CLI_JS: string;
	export const npm_command: string;
	export const npm_config_cache: string;
	export const npm_config_globalconfig: string;
	export const npm_config_global_prefix: string;
	export const npm_config_init_module: string;
	export const npm_config_local_prefix: string;
	export const npm_config_node_gyp: string;
	export const npm_config_noproxy: string;
	export const npm_config_npm_version: string;
	export const npm_config_prefix: string;
	export const npm_config_userconfig: string;
	export const npm_config_user_agent: string;
	export const npm_execpath: string;
	export const npm_lifecycle_event: string;
	export const npm_lifecycle_script: string;
	export const npm_node_execpath: string;
	export const npm_package_json: string;
	export const npm_package_name: string;
	export const npm_package_version: string;
	export const NPM_PREFIX_JS: string;
	export const NPM_PREFIX_NPM_CLI_JS: string;
	export const NUMBER_OF_PROCESSORS: string;
	export const ONEDRIVE: string;
	export const OPENSSL_CONF: string;
	export const OS: string;
	export const PATH: string;
	export const PATHEXT: string;
	export const PNPM_HOME: string;
	export const POSH_CURSOR_COLUMN: string;
	export const POSH_CURSOR_LINE: string;
	export const POSH_INSTALLER: string;
	export const POSH_SESSION_ID: string;
	export const POSH_SHELL: string;
	export const POSH_SHELL_VERSION: string;
	export const POSH_THEME: string;
	export const POSH_THEMES_PATH: string;
	export const POWERLINE_COMMAND: string;
	export const PROCESSOR_ARCHITECTURE: string;
	export const PROCESSOR_IDENTIFIER: string;
	export const PROCESSOR_LEVEL: string;
	export const PROCESSOR_REVISION: string;
	export const PROGRAMDATA: string;
	export const PROGRAMFILES: string;
	export const PROGRAMW6432: string;
	export const PROMPT: string;
	export const PSMODULEPATH: string;
	export const PUBLIC: string;
	export const PYENV_VIRTUALENV_DISABLE_PROMPT: string;
	export const SYSTEMDRIVE: string;
	export const SYSTEMROOT: string;
	export const TEMP: string;
	export const TERM_PROGRAM: string;
	export const TERM_PROGRAM_VERSION: string;
	export const TMP: string;
	export const USERDOMAIN: string;
	export const USERDOMAIN_ROAMINGPROFILE: string;
	export const USERNAME: string;
	export const USERPROFILE: string;
	export const VBOX_HWVIRTEX_IGNORE_SVM_IN_USE: string;
	export const VBOX_MSI_INSTALL_PATH: string;
	export const VIRTUAL_ENV_DISABLE_PROMPT: string;
	export const VSCODE_GIT_ASKPASS_EXTRA_ARGS: string;
	export const VSCODE_GIT_ASKPASS_MAIN: string;
	export const VSCODE_GIT_ASKPASS_NODE: string;
	export const VSCODE_GIT_IPC_HANDLE: string;
	export const VSCODE_INJECTION: string;
	export const VSCODE_PYTHON_AUTOACTIVATE_GUARD: string;
	export const WAILS_VITE_PORT: string;
	export const WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: string;
	export const WEBVIEW2_BROWSER_EXECUTABLE_FOLDER: string;
	export const WEBVIEW2_PIPE_FOR_SCRIPT_DEBUGGER: string;
	export const WEBVIEW2_RELEASE_CHANNEL_PREFERENCE: string;
	export const WEBVIEW2_USER_DATA_FOLDER: string;
	export const WINDIR: string;
}

/**
 * Similar to [`$env/static/private`](https://svelte.dev/docs/kit/$env-static-private), except that it only includes environment variables that begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) (which defaults to `PUBLIC_`), and can therefore safely be exposed to client-side code.
 * 
 * Values are replaced statically at build time.
 * 
 * ```ts
 * import { PUBLIC_BASE_URL } from '$env/static/public';
 * ```
 */
declare module '$env/static/public' {
	
}

/**
 * This module provides access to runtime environment variables, as defined by the platform you're running on. For example if you're using [`adapter-node`](https://github.com/sveltejs/kit/tree/main/packages/adapter-node) (or running [`vite preview`](https://svelte.dev/docs/kit/cli)), this is equivalent to `process.env`. This module only includes variables that _do not_ begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) _and do_ start with [`config.kit.env.privatePrefix`](https://svelte.dev/docs/kit/configuration#env) (if configured).
 * 
 * This module cannot be imported into client-side code.
 * 
 * ```ts
 * import { env } from '$env/dynamic/private';
 * console.log(env.DEPLOYMENT_SPECIFIC_VARIABLE);
 * ```
 * 
 * > [!NOTE] In `dev`, `$env/dynamic` always includes environment variables from `.env`. In `prod`, this behavior will depend on your adapter.
 */
declare module '$env/dynamic/private' {
	export const env: {
		ALLUSERSPROFILE: string;
		AMDRMPATH: string;
		ANDROID_HOME: string;
		APPDATA: string;
		BUN_INSPECT_CONNECT_TO: string;
		CABAL_DIR: string;
		CHOCOLATEYINSTALL: string;
		CHOCOLATEYLASTPATHUPDATE: string;
		CHROME_CRASHPAD_PIPE_NAME: string;
		COLOR: string;
		COLORTERM: string;
		COMMONPROGRAMFILES: string;
		COMMONPROGRAMW6432: string;
		COMPUTERNAME: string;
		COMSPEC: string;
		CONDA_PROMPT_MODIFIER: string;
		DRIVERDATA: string;
		EDITOR: string;
		FRONTEND_DEVSERVER_URL: string;
		GHCUP_INSTALL_BASE_PREFIX: string;
		GHCUP_MSYS2_ENV: string;
		GIT_ASKPASS: string;
		GOPATH: string;
		HAXEPATH: string;
		HOME: string;
		HOMEDRIVE: string;
		HOMEPATH: string;
		INIT_CWD: string;
		JAVA_HOME: string;
		LANG: string;
		LOCALAPPDATA: string;
		LOGONSERVER: string;
		NEKO_INSTPATH: string;
		NODE: string;
		NODE_ENV: string;
		NODE_EXE: string;
		NPM_CLI_JS: string;
		npm_command: string;
		npm_config_cache: string;
		npm_config_globalconfig: string;
		npm_config_global_prefix: string;
		npm_config_init_module: string;
		npm_config_local_prefix: string;
		npm_config_node_gyp: string;
		npm_config_noproxy: string;
		npm_config_npm_version: string;
		npm_config_prefix: string;
		npm_config_userconfig: string;
		npm_config_user_agent: string;
		npm_execpath: string;
		npm_lifecycle_event: string;
		npm_lifecycle_script: string;
		npm_node_execpath: string;
		npm_package_json: string;
		npm_package_name: string;
		npm_package_version: string;
		NPM_PREFIX_JS: string;
		NPM_PREFIX_NPM_CLI_JS: string;
		NUMBER_OF_PROCESSORS: string;
		ONEDRIVE: string;
		OPENSSL_CONF: string;
		OS: string;
		PATH: string;
		PATHEXT: string;
		PNPM_HOME: string;
		POSH_CURSOR_COLUMN: string;
		POSH_CURSOR_LINE: string;
		POSH_INSTALLER: string;
		POSH_SESSION_ID: string;
		POSH_SHELL: string;
		POSH_SHELL_VERSION: string;
		POSH_THEME: string;
		POSH_THEMES_PATH: string;
		POWERLINE_COMMAND: string;
		PROCESSOR_ARCHITECTURE: string;
		PROCESSOR_IDENTIFIER: string;
		PROCESSOR_LEVEL: string;
		PROCESSOR_REVISION: string;
		PROGRAMDATA: string;
		PROGRAMFILES: string;
		PROGRAMW6432: string;
		PROMPT: string;
		PSMODULEPATH: string;
		PUBLIC: string;
		PYENV_VIRTUALENV_DISABLE_PROMPT: string;
		SYSTEMDRIVE: string;
		SYSTEMROOT: string;
		TEMP: string;
		TERM_PROGRAM: string;
		TERM_PROGRAM_VERSION: string;
		TMP: string;
		USERDOMAIN: string;
		USERDOMAIN_ROAMINGPROFILE: string;
		USERNAME: string;
		USERPROFILE: string;
		VBOX_HWVIRTEX_IGNORE_SVM_IN_USE: string;
		VBOX_MSI_INSTALL_PATH: string;
		VIRTUAL_ENV_DISABLE_PROMPT: string;
		VSCODE_GIT_ASKPASS_EXTRA_ARGS: string;
		VSCODE_GIT_ASKPASS_MAIN: string;
		VSCODE_GIT_ASKPASS_NODE: string;
		VSCODE_GIT_IPC_HANDLE: string;
		VSCODE_INJECTION: string;
		VSCODE_PYTHON_AUTOACTIVATE_GUARD: string;
		WAILS_VITE_PORT: string;
		WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: string;
		WEBVIEW2_BROWSER_EXECUTABLE_FOLDER: string;
		WEBVIEW2_PIPE_FOR_SCRIPT_DEBUGGER: string;
		WEBVIEW2_RELEASE_CHANNEL_PREFERENCE: string;
		WEBVIEW2_USER_DATA_FOLDER: string;
		WINDIR: string;
		[key: `PUBLIC_${string}`]: undefined;
		[key: `${string}`]: string | undefined;
	}
}

/**
 * Similar to [`$env/dynamic/private`](https://svelte.dev/docs/kit/$env-dynamic-private), but only includes variables that begin with [`config.kit.env.publicPrefix`](https://svelte.dev/docs/kit/configuration#env) (which defaults to `PUBLIC_`), and can therefore safely be exposed to client-side code.
 * 
 * Note that public dynamic environment variables must all be sent from the server to the client, causing larger network requests — when possible, use `$env/static/public` instead.
 * 
 * ```ts
 * import { env } from '$env/dynamic/public';
 * console.log(env.PUBLIC_DEPLOYMENT_SPECIFIC_VARIABLE);
 * ```
 */
declare module '$env/dynamic/public' {
	export const env: {
		[key: `PUBLIC_${string}`]: string | undefined;
	}
}
