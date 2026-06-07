export function isMermaidLanguageClassName(className: string | undefined) {
  return className?.startsWith("language-mermaid") ?? false;
}

export function htmlOutputSandbox(activeContent: boolean): string {
  return activeContent ? "allow-scripts allow-same-origin" : "";
}

export function withVideoCapture(html: string): string {
  if (!html.includes("data-capture")) {
    return html;
  }

  return `${html}
<script>
(() => {
  const parsePositiveNumber = (value, fallback) => {
    const parsed = Number(value);
    return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
  };

  const recordCanvas = (canvas) => {
    if (canvas.dataset.juteCaptureStarted === "true") {
      return;
    }
    if (typeof canvas.captureStream !== "function" || typeof MediaRecorder === "undefined") {
      return;
    }

    canvas.dataset.juteCaptureStarted = "true";
    const fps = parsePositiveNumber(canvas.dataset.captureFps, 30);
    const durationSec = parsePositiveNumber(
      canvas.dataset.captureDurationSec ?? canvas.dataset.captureDuration,
      3,
    );
    const stream = canvas.captureStream(fps);
    const options = MediaRecorder.isTypeSupported?.("video/webm")
      ? { mimeType: "video/webm" }
      : undefined;
    const recorder = new MediaRecorder(stream, options);
    const chunks = [];

    recorder.addEventListener("dataavailable", (event) => {
      if (event.data && event.data.size > 0) {
        chunks.push(event.data);
      }
    });

    recorder.addEventListener("stop", () => {
      stream.getTracks().forEach((track) => track.stop());
      const blob = new Blob(chunks, { type: recorder.mimeType || "video/webm" });
      const reader = new FileReader();
      reader.addEventListener("loadend", () => {
        const result = typeof reader.result === "string" ? reader.result : "";
        const webm = result.includes(",") ? result.split(",", 2)[1] : result;
        window.parent.postMessage({
          type: "jute-video-capture",
          cellId: window.name || canvas.dataset.captureCellId || "",
          webm,
          duration_sec: durationSec,
        }, "*");
      });
      reader.readAsDataURL(blob);
    }, { once: true });

    recorder.start();
    window.setTimeout(() => {
      if (recorder.state !== "inactive") {
        recorder.stop();
      }
    }, durationSec * 1000);
  };

  const startCapture = () => {
    document
      .querySelectorAll('canvas[data-capture="true"], canvas[data-capture]')
      .forEach(recordCanvas);
  };

  if (document.readyState === "loading") {
    window.addEventListener("DOMContentLoaded", startCapture, { once: true });
  } else {
    startCapture();
  }
})();
</script>`;
}
