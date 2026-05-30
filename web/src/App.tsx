import { useState } from "react";
import { Chat } from "./components/Chat";
import { DataStudio } from "./components/DataStudio";
import "./styles.css";

type View = "chat" | "studio";

export default function App() {
  const [view, setView] = useState<View>("chat");

  return (
    <div className="shell">
      <nav className="tabs" role="tablist" aria-label="Demo">
        <button
          role="tab"
          aria-selected={view === "chat"}
          className={"tab" + (view === "chat" ? " is-active" : "")}
          onClick={() => setView("chat")}
        >
          Chat
        </button>
        <button
          role="tab"
          aria-selected={view === "studio"}
          className={"tab" + (view === "studio" ? " is-active" : "")}
          onClick={() => setView("studio")}
        >
          Data Studio
        </button>
        <span className="tabs-badge">block.kind.data</span>
      </nav>
      <div className="view">{view === "chat" ? <Chat /> : <DataStudio />}</div>
    </div>
  );
}
