import {
  ErrorBoundary as ReactErrorBoundary,
  type ErrorBoundaryProps,
  type FallbackProps,
} from "react-error-boundary";
import type { ComponentType } from "react";

export type { FallbackProps };

export const ErrorBoundary =
  ReactErrorBoundary as unknown as ComponentType<ErrorBoundaryProps>;
