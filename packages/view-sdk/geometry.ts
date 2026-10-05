export interface Rectangle {x:number;y:number;width:number;height:number}
export function intersect(a:Rectangle,b:Rectangle):Rectangle {
  const x=Math.max(a.x,b.x),y=Math.max(a.y,b.y);
  return {x,y,width:Math.max(0,Math.min(a.x+a.width,b.x+b.width)-x),height:Math.max(0,Math.min(a.y+a.height,b.y+b.height)-y)};
}
/** A host-owned child respects scrolling and clipping inside its parent renderer. */
export function slotClip(element:HTMLElement,rect:Rectangle):Rectangle {
  let clip=intersect(rect,{x:0,y:0,width:window.innerWidth,height:window.innerHeight});
  for(let parent=element.parentElement;parent;parent=parent.parentElement){
    const style=getComputedStyle(parent),x=/^(auto|scroll|hidden|clip)$/.test(style.overflowX),y=/^(auto|scroll|hidden|clip)$/.test(style.overflowY);
    if(!x&&!y)continue;
    const box=parent.getBoundingClientRect();
    clip=intersect(clip,{x:x?box.x+parent.clientLeft:clip.x,y:y?box.y+parent.clientTop:clip.y,
      width:x?parent.clientWidth:clip.width,height:y?parent.clientHeight:clip.height});
  }
  return clip;
}
