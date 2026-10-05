import { budget } from "../limits/policy";
export function max_active_view_roots():number { return budget("ui.view.roots"); }
export function max_view_frame_instances():number { return budget("ui.view.frames"); }
