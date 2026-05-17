/** Minimal ambient declarations so the fixture type-checks without @types/node or DOM lib. */
declare const process: {
    env: Record<string, string | undefined>;
};

declare const console: {
    log(...args: unknown[]): void;
    info(...args: unknown[]): void;
    warn(...args: unknown[]): void;
    error(...args: unknown[]): void;
    debug(...args: unknown[]): void;
};

declare const Buffer: {
    from(input: string, encoding?: string): { toString(encoding?: string): string };
};

declare function setTimeout(handler: (...args: unknown[]) => void, ms: number): unknown;
declare function clearTimeout(handle: unknown): void;
