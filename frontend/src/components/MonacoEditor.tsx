import { useEffect, useRef, useState } from "react";

declare global {
  interface Window {
    monaco?: any;
    require?: any;
  }
}

export interface EditorMarker {
  startLineNumber: number;
  startColumn: number;
  endLineNumber: number;
  endColumn: number;
  message: string;
  severity: "error" | "warning" | "info";
}

interface MonacoEditorProps {
  value: string;
  onChange: (val: string) => void;
  language?: string;
  height?: string;
  readOnly?: boolean;
  markers?: EditorMarker[];
}

// Global script loading state to prevent redundant injection
let monacoLoadingPromise: Promise<void> | null = null;

function loadMonaco(): Promise<void> {
  if (monacoLoadingPromise) {
    return monacoLoadingPromise;
  }

  monacoLoadingPromise = new Promise<void>((resolve, reject) => {
    // Check if monaco is already loaded
    if (window.monaco) {
      resolve();
      return;
    }

    // Check if the script loader is already in the DOM
    const loaderId = "monaco-amd-loader";
    let loaderScript = document.getElementById(loaderId) as HTMLScriptElement;

    if (!loaderScript) {
      loaderScript = document.createElement("script");
      loaderScript.id = loaderId;
      loaderScript.src = "https://cdnjs.cloudflare.com/ajax/libs/monaco-editor/0.45.0/min/vs/loader.js";
      loaderScript.async = true;
      document.body.appendChild(loaderScript);
    }

    const checkAndInit = () => {
      if (window.require) {
        window.require.config({
          paths: {
            vs: "https://cdnjs.cloudflare.com/ajax/libs/monaco-editor/0.45.0/min/vs",
          },
        });
        window.require(["vs/editor/editor.main"], () => {
          resolve();
        }, (err: any) => {
          reject(err);
        });
      } else {
        setTimeout(checkAndInit, 50);
      }
    };

    loaderScript.addEventListener("load", checkAndInit);
    loaderScript.addEventListener("error", (err) => {
      reject(new Error("Failed to load Monaco Editor loader script: " + err.message));
    });
  });

  return monacoLoadingPromise;
}

export default function MonacoEditor({
  value,
  onChange,
  language = "rust",
  height = "200px",
  readOnly = false,
  markers = [],
}: MonacoEditorProps): JSX.Element {
  const containerRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<any>(null);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const valueRef = useRef(value);

  // Sync ref with prop to avoid re-initializing editor on value changes
  valueRef.current = value;

  // 1. Load Monaco Editor Scripts from CDN
  useEffect(() => {
    loadMonaco()
      .then(() => {
        setLoaded(true);
      })
      .catch((err) => {
        setError(err.message || "Failed to load code editor.");
      });
  }, []);

  // 2. Initialize Monaco Instance when loaded
  useEffect(() => {
    if (!loaded || !containerRef.current || editorRef.current) return;

    const isDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    const initialTheme = isDark ? "vs-dark" : "vs";

    const editor = window.monaco.editor.create(containerRef.current, {
      value: valueRef.current,
      language,
      theme: initialTheme,
      automaticLayout: true,
      readOnly,
      minimap: { enabled: false },
      scrollBeyondLastLine: false,
      fontSize: 12,
      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace',
      lineNumbersMinChars: 3,
      padding: { top: 8, bottom: 8 },
      roundedSelection: true,
      scrollbar: {
        verticalScrollbarSize: 8,
        horizontalScrollbarSize: 8,
      },
    });

    editorRef.current = editor;

    // Listen for model change events
    const model = editor.getModel();
    const subscription = model.onDidChangeContent(() => {
      const currentValue = editor.getValue();
      if (currentValue !== valueRef.current) {
        valueRef.current = currentValue;
        onChange(currentValue);
      }
    });

    // Setup automatic layout resizing via ResizeObserver
    let resizeObserver: ResizeObserver | null = null;
    if (containerRef.current) {
      resizeObserver = new ResizeObserver(() => {
        editor.layout();
      });
      resizeObserver.observe(containerRef.current);
    }

    return () => {
      subscription.dispose();
      if (resizeObserver) {
        resizeObserver.disconnect();
      }
      editor.dispose();
      editorRef.current = null;
    };
  }, [loaded, language, readOnly, onChange]);

  // 3. Keep editor value synced with external value changes
  useEffect(() => {
    if (editorRef.current) {
      const currentValue = editorRef.current.getValue();
      if (value !== currentValue) {
        valueRef.current = value;
        editorRef.current.setValue(value);
      }
    }
  }, [value]);

  // 4. System Dark Mode Synchronizer
  useEffect(() => {
    if (!loaded) return;

    const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
    const themeListener = (e: MediaQueryListEvent) => {
      if (window.monaco) {
        window.monaco.editor.setTheme(e.matches ? "vs-dark" : "vs");
      }
    };

    mediaQuery.addEventListener("change", themeListener);
    return () => {
      mediaQuery.removeEventListener("change", themeListener);
    };
  }, [loaded]);

  // 5. Update Diagnostics markers
  useEffect(() => {
    if (!loaded || !editorRef.current || !window.monaco) return;

    const model = editorRef.current.getModel();
    if (!model) return;

    const monacoMarkers = markers.map((m) => ({
      startLineNumber: m.startLineNumber,
      startColumn: m.startColumn,
      endLineNumber: m.endLineNumber,
      endColumn: m.endColumn,
      message: m.message,
      severity:
        m.severity === "error"
          ? window.monaco.MarkerSeverity.Error
          : m.severity === "warning"
          ? window.monaco.MarkerSeverity.Warning
          : window.monaco.MarkerSeverity.Info,
    }));

    window.monaco.editor.setModelMarkers(model, "owner", monacoMarkers);
  }, [loaded, markers]);

  if (error) {
    return (
      <div className="monaco-editor-error">
        <p className="form-error">{error}</p>
        <textarea
          style={{ width: "100%", height, background: "transparent", border: "1px solid rgba(127,127,127,0.3)" }}
          value={value}
          onChange={(e) => onChange(e.target.value)}
        />
      </div>
    );
  }

  return (
    <div className="monaco-editor-outer">
      {!loaded && (
        <div className="monaco-editor-loader" style={{ height }}>
          <div className="spinner"></div>
          <span>Loading Editor...</span>
        </div>
      )}
      <div
        ref={containerRef}
        className="monaco-editor-inner"
        style={{
          height,
          display: loaded ? "block" : "none",
        }}
      />
    </div>
  );
}
