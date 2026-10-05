import {useCallback,useState} from "react";

/** User-triggered copies report unavailable permissions without logging the copied data. */
export function useClipboard() {
  const [notice,setNotice]=useState<string>();
  const copy=useCallback(async(text:string)=>{
    try {
      if(!navigator.clipboard)throw new Error("Clipboard is unavailable");
      await navigator.clipboard.writeText(text);
      setNotice("Copied");
    } catch(error) {setNotice(`Copy failed · ${error instanceof Error ? error.message : String(error)}`);}
  },[]);
  return {copy,notice};
}
