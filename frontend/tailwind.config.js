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
                primary: "var(--color-primary)",
                secondary: "var(--color-secondary)",
                accent: "var(--color-accent)",
                ink: "var(--color-ink)",
                inkLight: "var(--color-inkLight)",
            },
        },
    },
    safelist: ["bg-slate-700", "text-3xl", "font-bold"],
};
