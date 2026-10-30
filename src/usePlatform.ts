// 判断当前是桌面还是移动，决定用哪套组件库。

import { useEffect, useState } from "react";

export type FormFactor = "desktop" | "mobile";

/// 从 User-Agent 与视口宽度共同判断。
///
/// 只看宽度会在桌面端拖窄窗口时误判，只看 UA 会在
/// 平板/桌面模式的移动浏览器上误判，所以两个条件取或。
export function detectFormFactor(): FormFactor {
  if (typeof window === "undefined") return "desktop";

  const ua = navigator.userAgent.toLowerCase();
  const mobileUa = /android|iphone|ipad|ipod|mobile/.test(ua);

  // Tauri 在移动端的 UA 里会带平台标识
  const narrow = window.matchMedia("(max-width: 720px)").matches;

  return mobileUa || narrow ? "mobile" : "desktop";
}

export function useFormFactor(): FormFactor {
  const [factor, setFactor] = useState<FormFactor>(detectFormFactor);

  useEffect(() => {
    const mq = window.matchMedia("(max-width: 720px)");
    const onChange = () => setFactor(detectFormFactor());
    mq.addEventListener("change", onChange);
    window.addEventListener("resize", onChange);
    return () => {
      mq.removeEventListener("change", onChange);
      window.removeEventListener("resize", onChange);
    };
  }, []);

  return factor;
}
