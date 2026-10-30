import { useFormFactor } from "./usePlatform";
import { useKanami } from "./useKanami";
import DesktopApp from "./desktop/DesktopApp";
import MobileApp from "./mobile/MobileApp";

/// 应用根组件。
///
/// 桌面用 antd，移动用 antd-mobile —— 两套界面共享同一个 useKanami 状态，
/// 业务逻辑一份，只在组件层分叉。
export default function App() {
  const factor = useFormFactor();
  const kanami = useKanami();

  return factor === "mobile" ? <MobileApp k={kanami} /> : <DesktopApp k={kanami} />;
}
