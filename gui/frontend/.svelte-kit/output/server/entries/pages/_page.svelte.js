import { c as create_ssr_component, e as escape, d as add_attribute } from "../../chunks/ssr.js";
import { Events } from "@wailsio/runtime";
const Page = create_ssr_component(($$result, $$props, $$bindings, slots) => {
  let name = "";
  let result = "Please enter your name below 👇";
  let time = "Listening for Time event...";
  Events.On("time", (timeValue) => {
    time = timeValue.data;
  });
  return `<div class="container"><div data-svelte-h="svelte-hpgj0w"><span data-wml-openurl="https://wails.io"><img src="/wails.png" class="logo" alt="Wails logo"></span> <span data-wml-openurl="https://svelte.dev"><img src="/svelte.svg" class="logo svelte" alt="Svelte logo"></span></div> <h1 data-svelte-h="svelte-1fewmnq">Wails + Svelte</h1> <div aria-label="result" class="result">${escape(result)}</div> <div class="card"><div class="input-box"><input aria-label="input" class="input" type="text" autocomplete="off"${add_attribute("value", name)}> <button aria-label="greet-btn" class="btn" data-svelte-h="svelte-13v3c7p">Greet</button></div></div> <div class="footer"><div data-svelte-h="svelte-s5xnoy"><p>Click on the Wails logo to learn more</p></div> <div><p>${escape(time)}</p></div></div> </div>`;
});
export {
  Page as default
};
