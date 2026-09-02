import AOS from "aos";

/** Thin wrapper around the AOS motion library used for scroll reveals. */
export function initMotion(): void {
  AOS.init({ duration: 500, easing: "ease-out-cubic", once: true, offset: 30 });
}

/** Call after (re)rendering dynamic content so new nodes animate in. */
export function refreshMotion(): void {
  AOS.refreshHard();
}
