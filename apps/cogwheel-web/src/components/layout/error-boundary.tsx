import React from "react";
import { RotateCwIcon, TriangleAlertIcon } from "lucide-react";
import { Button } from "@/components/ui/button";

type Props = { children: React.ReactNode };
type State = { error: Error | null };

/**
 * Last line of defence. Resetting state alone often re-throws immediately, so
 * the primary action reloads the document; retrying in place is offered as the
 * lighter option for transient render faults.
 */
export class ErrorBoundary extends React.Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo): void {
    console.error("Cogwheel UI crashed", error, info.componentStack);
  }

  render(): React.ReactNode {
    const { error } = this.state;
    if (!error) return this.props.children;

    return (
      <div className="flex min-h-svh items-center justify-center bg-background p-6">
        <div className="w-full max-w-md rounded-xl border border-border bg-card p-6">
          <TriangleAlertIcon aria-hidden className="size-5 text-destructive-foreground" />
          <h1 className="mt-3 font-semibold text-foreground text-lg">The interface stopped rendering</h1>
          <p className="mt-1 text-muted-foreground text-sm">
            DNS filtering on the appliance is unaffected — this is a fault in the control-plane UI only.
          </p>
          <pre className="mt-4 overflow-x-auto rounded-lg bg-muted p-3 font-mono text-xs">
            {error.message}
          </pre>
          <div className="mt-4 flex gap-2">
            <Button onClick={() => window.location.reload()}>
              <RotateCwIcon aria-hidden />
              Reload
            </Button>
            <Button onClick={() => this.setState({ error: null })} variant="outline">
              Try again
            </Button>
          </div>
        </div>
      </div>
    );
  }
}
