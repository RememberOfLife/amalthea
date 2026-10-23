// Presentation follows explicit lifecycle phases, never human-readable status text.
export function transferProgress(job, serverFile) {
    if (
        job.kind === "upload" &&
        job.complete &&
        serverFile?.status === "processing"
    ) {
        job = {
            ...job,
            active: true,
            complete: false,
            phase: "server",
            status: "Server processing",
        };
    } else if (
        job.kind === "upload" &&
        job.complete &&
        serverFile?.status === "failed"
    ) {
        job = {
            ...job,
            active: false,
            complete: false,
            phase: "error",
            status: serverFile.processing_error || "Server processing failed",
        };
    }
    const active = !!job.active && !job.pausing && !job.complete;
    const indeterminate =
        active && ["preparing", "verifying", "server"].includes(job.phase);
    const percent =
        indeterminate || job.complete
            ? 100
            : job.size > 0
              ? Math.max(0, Math.min(100, (100 * (job.offset || 0)) / job.size))
              : 0;
    const color =
        job.phase === "error"
            ? "danger"
            : job.phase === "conflict"
              ? "warning"
              : job.complete
                ? "success"
                : job.pausing
                  ? "warning"
                  : !active
                    ? "secondary"
                    : job.phase === "server"
                      ? "info"
                      : job.phase === "transfer"
                        ? "success"
                        : "primary";
    return {
        active,
        indeterminate,
        percent,
        color,
        labelOnFill: percent >= 50,
        status: job.status,
    };
}
