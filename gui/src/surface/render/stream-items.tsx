import {createContext} from "react";
/** Declared item keys of the displayed list, by position; arbitrary field names never imply identity. */
export const StreamItemsContext=createContext<ReadonlyMap<number,string>|undefined>(undefined);
/** Builtin Docker identities and validated log mappings, read by the same rule as the log view's rows. */
export {declaredItemKeys as streamItemKeys} from "./log-identity";
