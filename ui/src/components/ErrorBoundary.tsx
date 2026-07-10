import { Component, type ReactNode } from "react";

interface Props {
  children: ReactNode;
  /** When this value changes, the error state resets — pass `page`
   * so navigating away from a crashed page recovers automatically. */
  resetKey?: string;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidUpdate(prevProps: Props) {
    if (
      this.state.hasError &&
      this.props.resetKey !== undefined &&
      this.props.resetKey !== prevProps.resetKey
    ) {
      this.setState({ hasError: false, error: null });
    }
  }

  render() {
    if (this.state.hasError) {
      return (
        <div className="page" style={{ padding: "2rem", textAlign: "center" }}>
          <h2>Something went wrong</h2>
          <p style={{ color: "var(--muted)", fontSize: "0.875rem" }}>
            {this.state.error?.message}
          </p>
          <button
            className="btn"
            onClick={() => window.location.reload()}
            style={{ marginTop: "1rem" }}
          >
            Reload
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
