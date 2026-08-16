import React from "react";
import { errorMessage } from "@/lib/api";

export type AsyncState<T> = {
  data: T | null;
  loading: boolean;
  error: string | null;
};

/**
 * On-demand loader for data that does not belong in the shared polled snapshot
 * — screen-local lists such as audit events, rulesets or detections. Exposes
 * the loading/error/empty triple every screen has to render explicitly.
 *
 * `key` is the re-fetch trigger: change it and the loader runs again. It is a
 * plain string rather than a dependency array so the effect's own dependency
 * list stays statically checkable.
 */
export function useAsync<T>(
  key: string,
  loader: (signal: AbortSignal) => Promise<T>,
): AsyncState<T> & { reload: () => void } {
  const [state, setState] = React.useState<AsyncState<T>>({ data: null, loading: true, error: null });
  const [nonce, setNonce] = React.useState(0);

  // The loader closure is recreated on every render at most call sites; only
  // `key` should decide when it actually runs.
  const loaderRef = React.useRef(loader);
  loaderRef.current = loader;

  React.useEffect(() => {
    const controller = new AbortController();
    let active = true;

    setState((current) => ({ ...current, loading: true }));
    loaderRef
      .current(controller.signal)
      .then((data) => {
        if (active) setState({ data, loading: false, error: null });
      })
      .catch((cause: unknown) => {
        if (!active || (cause instanceof DOMException && cause.name === "AbortError")) return;
        // Keep the previous payload on screen; a refresh failure is not a reason
        // to throw away data the operator was already reading.
        setState((current) => ({ ...current, loading: false, error: errorMessage(cause) }));
      });

    return () => {
      active = false;
      controller.abort();
    };
  }, [key, nonce]);

  const reload = React.useCallback(() => setNonce((value) => value + 1), []);

  return { ...state, reload };
}
