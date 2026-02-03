const __vite__mapDeps=(i,m=__vite__mapDeps,d=(m.f||(m.f=["../nodes/0.DCsD76Is.js","../chunks/BOkfkSpx.js","../chunks/bvTm1Fpp.js","../nodes/1.BzXTrDck.js","../chunks/DH-iivNG.js","../nodes/2.B6muBM1K.js"])))=>i.map(i=>d[i]);
import { s as safe_not_equal, d as afterUpdate, o as onMount, t as tick, e as binding_callbacks } from "../chunks/BOkfkSpx.js";
import { S as SvelteComponent, i as init, d as detach, t as transition_out, a as transition_in, m as group_outros, n as check_outros, b as insert_hydration, h as claim_space, o as empty, l as space, p as construct_svelte_component, q as destroy_component, r as create_component, u as mount_component, v as claim_component, w as attr, x as set_style, e as claim_element, f as children, j as element, s as set_data, g as claim_text, k as text } from "../chunks/bvTm1Fpp.js";
const scriptRel = "modulepreload";
const assetsURL = function(dep, importerUrl) {
  return new URL(dep, importerUrl).href;
};
const seen = {};
const __vitePreload = function preload(baseModule, deps, importerUrl) {
  let promise = Promise.resolve();
  if (deps && deps.length > 0) {
    const links = document.getElementsByTagName("link");
    const cspNonceMeta = document.querySelector(
      "meta[property=csp-nonce]"
    );
    const cspNonce = (cspNonceMeta == null ? void 0 : cspNonceMeta.nonce) || (cspNonceMeta == null ? void 0 : cspNonceMeta.getAttribute("nonce"));
    promise = Promise.allSettled(
      deps.map((dep) => {
        dep = assetsURL(dep, importerUrl);
        if (dep in seen) return;
        seen[dep] = true;
        const isCss = dep.endsWith(".css");
        const cssSelector = isCss ? '[rel="stylesheet"]' : "";
        const isBaseRelative = !!importerUrl;
        if (isBaseRelative) {
          for (let i = links.length - 1; i >= 0; i--) {
            const link2 = links[i];
            if (link2.href === dep && (!isCss || link2.rel === "stylesheet")) {
              return;
            }
          }
        } else if (document.querySelector(`link[href="${dep}"]${cssSelector}`)) {
          return;
        }
        const link = document.createElement("link");
        link.rel = isCss ? "stylesheet" : scriptRel;
        if (!isCss) {
          link.as = "script";
        }
        link.crossOrigin = "";
        link.href = dep;
        if (cspNonce) {
          link.setAttribute("nonce", cspNonce);
        }
        document.head.appendChild(link);
        if (isCss) {
          return new Promise((res, rej) => {
            link.addEventListener("load", res);
            link.addEventListener(
              "error",
              () => rej(new Error(`Unable to preload CSS for ${dep}`))
            );
          });
        }
      })
    );
  }
  function handlePreloadError(err) {
    const e = new Event("vite:preloadError", {
      cancelable: true
    });
    e.payload = err;
    window.dispatchEvent(e);
    if (!e.defaultPrevented) {
      throw err;
    }
  }
  return promise.then((res) => {
    for (const item of res || []) {
      if (item.status !== "rejected") continue;
      handlePreloadError(item.reason);
    }
    return baseModule().catch(handlePreloadError);
  });
};
const matchers = {};
function create_else_block(ctx) {
  let switch_instance;
  let switch_instance_anchor;
  let current;
  var switch_value = (
    /*constructors*/
    ctx[2][0]
  );
  function switch_props(ctx2, dirty) {
    let switch_instance_props = {
      data: (
        /*data_0*/
        ctx2[4]
      ),
      form: (
        /*form*/
        ctx2[3]
      ),
      params: (
        /*page*/
        ctx2[1].params
      )
    };
    return { props: switch_instance_props };
  }
  if (switch_value) {
    switch_instance = construct_svelte_component(switch_value, switch_props(ctx));
    ctx[12](switch_instance);
  }
  return {
    c() {
      if (switch_instance) create_component(switch_instance.$$.fragment);
      switch_instance_anchor = empty();
    },
    l(nodes2) {
      if (switch_instance) claim_component(switch_instance.$$.fragment, nodes2);
      switch_instance_anchor = empty();
    },
    m(target, anchor) {
      if (switch_instance) mount_component(switch_instance, target, anchor);
      insert_hydration(target, switch_instance_anchor, anchor);
      current = true;
    },
    p(ctx2, dirty) {
      if (dirty & /*constructors*/
      4 && switch_value !== (switch_value = /*constructors*/
      ctx2[2][0])) {
        if (switch_instance) {
          group_outros();
          const old_component = switch_instance;
          transition_out(old_component.$$.fragment, 1, 0, () => {
            destroy_component(old_component, 1);
          });
          check_outros();
        }
        if (switch_value) {
          switch_instance = construct_svelte_component(switch_value, switch_props(ctx2));
          ctx2[12](switch_instance);
          create_component(switch_instance.$$.fragment);
          transition_in(switch_instance.$$.fragment, 1);
          mount_component(switch_instance, switch_instance_anchor.parentNode, switch_instance_anchor);
        } else {
          switch_instance = null;
        }
      } else if (switch_value) {
        const switch_instance_changes = {};
        if (dirty & /*data_0*/
        16) switch_instance_changes.data = /*data_0*/
        ctx2[4];
        if (dirty & /*form*/
        8) switch_instance_changes.form = /*form*/
        ctx2[3];
        if (dirty & /*page*/
        2) switch_instance_changes.params = /*page*/
        ctx2[1].params;
        switch_instance.$set(switch_instance_changes);
      }
    },
    i(local) {
      if (current) return;
      if (switch_instance) transition_in(switch_instance.$$.fragment, local);
      current = true;
    },
    o(local) {
      if (switch_instance) transition_out(switch_instance.$$.fragment, local);
      current = false;
    },
    d(detaching) {
      if (detaching) {
        detach(switch_instance_anchor);
      }
      ctx[12](null);
      if (switch_instance) destroy_component(switch_instance, detaching);
    }
  };
}
function create_if_block_2(ctx) {
  let switch_instance;
  let switch_instance_anchor;
  let current;
  var switch_value = (
    /*constructors*/
    ctx[2][0]
  );
  function switch_props(ctx2, dirty) {
    let switch_instance_props = {
      data: (
        /*data_0*/
        ctx2[4]
      ),
      params: (
        /*page*/
        ctx2[1].params
      ),
      $$slots: { default: [create_default_slot] },
      $$scope: { ctx: ctx2 }
    };
    return { props: switch_instance_props };
  }
  if (switch_value) {
    switch_instance = construct_svelte_component(switch_value, switch_props(ctx));
    ctx[11](switch_instance);
  }
  return {
    c() {
      if (switch_instance) create_component(switch_instance.$$.fragment);
      switch_instance_anchor = empty();
    },
    l(nodes2) {
      if (switch_instance) claim_component(switch_instance.$$.fragment, nodes2);
      switch_instance_anchor = empty();
    },
    m(target, anchor) {
      if (switch_instance) mount_component(switch_instance, target, anchor);
      insert_hydration(target, switch_instance_anchor, anchor);
      current = true;
    },
    p(ctx2, dirty) {
      if (dirty & /*constructors*/
      4 && switch_value !== (switch_value = /*constructors*/
      ctx2[2][0])) {
        if (switch_instance) {
          group_outros();
          const old_component = switch_instance;
          transition_out(old_component.$$.fragment, 1, 0, () => {
            destroy_component(old_component, 1);
          });
          check_outros();
        }
        if (switch_value) {
          switch_instance = construct_svelte_component(switch_value, switch_props(ctx2));
          ctx2[11](switch_instance);
          create_component(switch_instance.$$.fragment);
          transition_in(switch_instance.$$.fragment, 1);
          mount_component(switch_instance, switch_instance_anchor.parentNode, switch_instance_anchor);
        } else {
          switch_instance = null;
        }
      } else if (switch_value) {
        const switch_instance_changes = {};
        if (dirty & /*data_0*/
        16) switch_instance_changes.data = /*data_0*/
        ctx2[4];
        if (dirty & /*page*/
        2) switch_instance_changes.params = /*page*/
        ctx2[1].params;
        if (dirty & /*$$scope, constructors, data_1, form, page, components*/
        8239) {
          switch_instance_changes.$$scope = { dirty, ctx: ctx2 };
        }
        switch_instance.$set(switch_instance_changes);
      }
    },
    i(local) {
      if (current) return;
      if (switch_instance) transition_in(switch_instance.$$.fragment, local);
      current = true;
    },
    o(local) {
      if (switch_instance) transition_out(switch_instance.$$.fragment, local);
      current = false;
    },
    d(detaching) {
      if (detaching) {
        detach(switch_instance_anchor);
      }
      ctx[11](null);
      if (switch_instance) destroy_component(switch_instance, detaching);
    }
  };
}
function create_default_slot(ctx) {
  let switch_instance;
  let switch_instance_anchor;
  let current;
  var switch_value = (
    /*constructors*/
    ctx[2][1]
  );
  function switch_props(ctx2, dirty) {
    let switch_instance_props = {
      data: (
        /*data_1*/
        ctx2[5]
      ),
      form: (
        /*form*/
        ctx2[3]
      ),
      params: (
        /*page*/
        ctx2[1].params
      )
    };
    return { props: switch_instance_props };
  }
  if (switch_value) {
    switch_instance = construct_svelte_component(switch_value, switch_props(ctx));
    ctx[10](switch_instance);
  }
  return {
    c() {
      if (switch_instance) create_component(switch_instance.$$.fragment);
      switch_instance_anchor = empty();
    },
    l(nodes2) {
      if (switch_instance) claim_component(switch_instance.$$.fragment, nodes2);
      switch_instance_anchor = empty();
    },
    m(target, anchor) {
      if (switch_instance) mount_component(switch_instance, target, anchor);
      insert_hydration(target, switch_instance_anchor, anchor);
      current = true;
    },
    p(ctx2, dirty) {
      if (dirty & /*constructors*/
      4 && switch_value !== (switch_value = /*constructors*/
      ctx2[2][1])) {
        if (switch_instance) {
          group_outros();
          const old_component = switch_instance;
          transition_out(old_component.$$.fragment, 1, 0, () => {
            destroy_component(old_component, 1);
          });
          check_outros();
        }
        if (switch_value) {
          switch_instance = construct_svelte_component(switch_value, switch_props(ctx2));
          ctx2[10](switch_instance);
          create_component(switch_instance.$$.fragment);
          transition_in(switch_instance.$$.fragment, 1);
          mount_component(switch_instance, switch_instance_anchor.parentNode, switch_instance_anchor);
        } else {
          switch_instance = null;
        }
      } else if (switch_value) {
        const switch_instance_changes = {};
        if (dirty & /*data_1*/
        32) switch_instance_changes.data = /*data_1*/
        ctx2[5];
        if (dirty & /*form*/
        8) switch_instance_changes.form = /*form*/
        ctx2[3];
        if (dirty & /*page*/
        2) switch_instance_changes.params = /*page*/
        ctx2[1].params;
        switch_instance.$set(switch_instance_changes);
      }
    },
    i(local) {
      if (current) return;
      if (switch_instance) transition_in(switch_instance.$$.fragment, local);
      current = true;
    },
    o(local) {
      if (switch_instance) transition_out(switch_instance.$$.fragment, local);
      current = false;
    },
    d(detaching) {
      if (detaching) {
        detach(switch_instance_anchor);
      }
      ctx[10](null);
      if (switch_instance) destroy_component(switch_instance, detaching);
    }
  };
}
function create_if_block(ctx) {
  let div;
  let if_block = (
    /*navigated*/
    ctx[7] && create_if_block_1(ctx)
  );
  return {
    c() {
      div = element("div");
      if (if_block) if_block.c();
      this.h();
    },
    l(nodes2) {
      div = claim_element(nodes2, "DIV", {
        id: true,
        "aria-live": true,
        "aria-atomic": true,
        style: true
      });
      var div_nodes = children(div);
      if (if_block) if_block.l(div_nodes);
      div_nodes.forEach(detach);
      this.h();
    },
    h() {
      attr(div, "id", "svelte-announcer");
      attr(div, "aria-live", "assertive");
      attr(div, "aria-atomic", "true");
      set_style(div, "position", "absolute");
      set_style(div, "left", "0");
      set_style(div, "top", "0");
      set_style(div, "clip", "rect(0 0 0 0)");
      set_style(div, "clip-path", "inset(50%)");
      set_style(div, "overflow", "hidden");
      set_style(div, "white-space", "nowrap");
      set_style(div, "width", "1px");
      set_style(div, "height", "1px");
    },
    m(target, anchor) {
      insert_hydration(target, div, anchor);
      if (if_block) if_block.m(div, null);
    },
    p(ctx2, dirty) {
      if (
        /*navigated*/
        ctx2[7]
      ) {
        if (if_block) {
          if_block.p(ctx2, dirty);
        } else {
          if_block = create_if_block_1(ctx2);
          if_block.c();
          if_block.m(div, null);
        }
      } else if (if_block) {
        if_block.d(1);
        if_block = null;
      }
    },
    d(detaching) {
      if (detaching) {
        detach(div);
      }
      if (if_block) if_block.d();
    }
  };
}
function create_if_block_1(ctx) {
  let t;
  return {
    c() {
      t = text(
        /*title*/
        ctx[8]
      );
    },
    l(nodes2) {
      t = claim_text(
        nodes2,
        /*title*/
        ctx[8]
      );
    },
    m(target, anchor) {
      insert_hydration(target, t, anchor);
    },
    p(ctx2, dirty) {
      if (dirty & /*title*/
      256) set_data(
        t,
        /*title*/
        ctx2[8]
      );
    },
    d(detaching) {
      if (detaching) {
        detach(t);
      }
    }
  };
}
function create_fragment(ctx) {
  let current_block_type_index;
  let if_block0;
  let t;
  let if_block1_anchor;
  let current;
  const if_block_creators = [create_if_block_2, create_else_block];
  const if_blocks = [];
  function select_block_type(ctx2, dirty) {
    if (
      /*constructors*/
      ctx2[2][1]
    ) return 0;
    return 1;
  }
  current_block_type_index = select_block_type(ctx);
  if_block0 = if_blocks[current_block_type_index] = if_block_creators[current_block_type_index](ctx);
  let if_block1 = (
    /*mounted*/
    ctx[6] && create_if_block(ctx)
  );
  return {
    c() {
      if_block0.c();
      t = space();
      if (if_block1) if_block1.c();
      if_block1_anchor = empty();
    },
    l(nodes2) {
      if_block0.l(nodes2);
      t = claim_space(nodes2);
      if (if_block1) if_block1.l(nodes2);
      if_block1_anchor = empty();
    },
    m(target, anchor) {
      if_blocks[current_block_type_index].m(target, anchor);
      insert_hydration(target, t, anchor);
      if (if_block1) if_block1.m(target, anchor);
      insert_hydration(target, if_block1_anchor, anchor);
      current = true;
    },
    p(ctx2, [dirty]) {
      let previous_block_index = current_block_type_index;
      current_block_type_index = select_block_type(ctx2);
      if (current_block_type_index === previous_block_index) {
        if_blocks[current_block_type_index].p(ctx2, dirty);
      } else {
        group_outros();
        transition_out(if_blocks[previous_block_index], 1, 1, () => {
          if_blocks[previous_block_index] = null;
        });
        check_outros();
        if_block0 = if_blocks[current_block_type_index];
        if (!if_block0) {
          if_block0 = if_blocks[current_block_type_index] = if_block_creators[current_block_type_index](ctx2);
          if_block0.c();
        } else {
          if_block0.p(ctx2, dirty);
        }
        transition_in(if_block0, 1);
        if_block0.m(t.parentNode, t);
      }
      if (
        /*mounted*/
        ctx2[6]
      ) {
        if (if_block1) {
          if_block1.p(ctx2, dirty);
        } else {
          if_block1 = create_if_block(ctx2);
          if_block1.c();
          if_block1.m(if_block1_anchor.parentNode, if_block1_anchor);
        }
      } else if (if_block1) {
        if_block1.d(1);
        if_block1 = null;
      }
    },
    i(local) {
      if (current) return;
      transition_in(if_block0);
      current = true;
    },
    o(local) {
      transition_out(if_block0);
      current = false;
    },
    d(detaching) {
      if (detaching) {
        detach(t);
        detach(if_block1_anchor);
      }
      if_blocks[current_block_type_index].d(detaching);
      if (if_block1) if_block1.d(detaching);
    }
  };
}
function instance($$self, $$props, $$invalidate) {
  let { stores } = $$props;
  let { page } = $$props;
  let { constructors } = $$props;
  let { components = [] } = $$props;
  let { form } = $$props;
  let { data_0 = null } = $$props;
  let { data_1 = null } = $$props;
  afterUpdate(stores.page.notify);
  let mounted = false;
  let navigated = false;
  let title = null;
  onMount(() => {
    const unsubscribe = stores.page.subscribe(() => {
      if (mounted) {
        $$invalidate(7, navigated = true);
        tick().then(() => {
          $$invalidate(8, title = document.title || "untitled page");
        });
      }
    });
    $$invalidate(6, mounted = true);
    return unsubscribe;
  });
  function switch_instance_binding($$value) {
    binding_callbacks[$$value ? "unshift" : "push"](() => {
      components[1] = $$value;
      $$invalidate(0, components);
    });
  }
  function switch_instance_binding_1($$value) {
    binding_callbacks[$$value ? "unshift" : "push"](() => {
      components[0] = $$value;
      $$invalidate(0, components);
    });
  }
  function switch_instance_binding_2($$value) {
    binding_callbacks[$$value ? "unshift" : "push"](() => {
      components[0] = $$value;
      $$invalidate(0, components);
    });
  }
  $$self.$$set = ($$props2) => {
    if ("stores" in $$props2) $$invalidate(9, stores = $$props2.stores);
    if ("page" in $$props2) $$invalidate(1, page = $$props2.page);
    if ("constructors" in $$props2) $$invalidate(2, constructors = $$props2.constructors);
    if ("components" in $$props2) $$invalidate(0, components = $$props2.components);
    if ("form" in $$props2) $$invalidate(3, form = $$props2.form);
    if ("data_0" in $$props2) $$invalidate(4, data_0 = $$props2.data_0);
    if ("data_1" in $$props2) $$invalidate(5, data_1 = $$props2.data_1);
  };
  $$self.$$.update = () => {
    if ($$self.$$.dirty & /*stores, page*/
    514) {
      stores.page.set(page);
    }
  };
  return [
    components,
    page,
    constructors,
    form,
    data_0,
    data_1,
    mounted,
    navigated,
    title,
    stores,
    switch_instance_binding,
    switch_instance_binding_1,
    switch_instance_binding_2
  ];
}
class Root extends SvelteComponent {
  constructor(options) {
    super();
    init(this, options, instance, create_fragment, safe_not_equal, {
      stores: 9,
      page: 1,
      constructors: 2,
      components: 0,
      form: 3,
      data_0: 4,
      data_1: 5
    });
  }
}
const nodes = [
  () => __vitePreload(() => import("../nodes/0.DCsD76Is.js"), true ? __vite__mapDeps([0,1,2]) : void 0, import.meta.url),
  () => __vitePreload(() => import("../nodes/1.BzXTrDck.js"), true ? __vite__mapDeps([3,1,2,4]) : void 0, import.meta.url),
  () => __vitePreload(() => import("../nodes/2.B6muBM1K.js"), true ? __vite__mapDeps([5,1,2]) : void 0, import.meta.url)
];
const server_loads = [];
const dictionary = {
  "/": [2]
};
const hooks = {
  handleError: ({ error }) => {
    console.error(error);
  },
  reroute: () => {
  },
  transport: {}
};
const decoders = Object.fromEntries(Object.entries(hooks.transport).map(([k, v]) => [k, v.decode]));
const encoders = Object.fromEntries(Object.entries(hooks.transport).map(([k, v]) => [k, v.encode]));
const hash = false;
const decode = (type, value) => decoders[type](value);
export {
  decode,
  decoders,
  dictionary,
  encoders,
  hash,
  hooks,
  matchers,
  nodes,
  Root as root,
  server_loads
};
