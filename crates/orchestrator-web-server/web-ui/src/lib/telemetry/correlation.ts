export const AO_CORRELATION_HEADER = "X-AO-Correlation-ID";

const CORRELATION_PREFIX = "ao-web";
let correlationSequence = 0;

export function generateCorrelationId(): string {
  correlationSequence += 1;

  const timeSegment = Date.now().toString(36);
  const sequenceSegment = correlationSequence.toString(36);
  const randomSegment = Math.random().toString(36).slice(2, 10);

  return `${CORRELATION_PREFIX}-${timeSegment}-${sequenceSegment}-${randomSegment}`;
}

export function normalizeCorrelationId(value: string | null | undefined): string | null {
  if (typeof value !== "string") {
    return null;
  }

  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

export function resolveCorrelationId(
  preferred: string | null | undefined,
  fallback: string | null | undefined,
): string | null {
  return normalizeCorrelationId(preferred) ?? normalizeCorrelationId(fallback);
}

export function resetCorrelationSequenceForTests(): void {
  correlationSequence = 0;
}
