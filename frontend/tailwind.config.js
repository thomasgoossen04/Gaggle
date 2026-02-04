/** @type {import('tailwindcss').Config} */
module.exports = {
    content: [
        "./index.html",
        "./src/**/*.rs",
    ],
    theme: {
        extend: {
            fontFamily: {
                sans: ["Space Grotesk", "ui-sans-serif", "system-ui", "sans-serif"],
            },
            colors: {
                primary: "#FF7F11",
                secondary: "#ACBFA4",
                accent: "#E2E8CE",
                ink: "#262626",
                inkLight: "#5a5a5a",
            },
        },
    },
    safelist: ["bg-slate-700", "text-3xl", "font-bold"],
};
