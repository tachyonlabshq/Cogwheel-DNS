import { createContext, useContext } from "react";

export type InspectorContextValue = { inspect: (domain: string) => void };

export const InspectorContext = createContext<InspectorContextValue | null>(null);

/** Any domain shown anywhere in the app can hand itself to the inspector. */
export function useDomainInspector(): InspectorContextValue {
  const value = useContext(InspectorContext);
  if (!value) throw new Error("useDomainInspector must be used inside <DomainInspectorProvider>");
  return value;
}
