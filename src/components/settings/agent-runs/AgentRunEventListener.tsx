import { useEffect } from "react";
import { events } from "@/bindings";
import { useAgentRunEventsStore } from "../../../stores/agentRunEventsStore";

/**
 * Global listener for `agent-run-event`, mounted once at the App root — same
 * pattern as `MeetingDetectionListener`. `AgentRunsSettings` (the panel that
 * reads `useAgentRunEventsStore`) unmounts whenever a different settings
 * section is active, so a listener owned by it would drop every event —
 * including a parked `PermissionRequest`, which has no other way to resolve
 * besides Stop — for as long as the user stayed off that one tab (Task 11
 * review, Critical 2). This component only feeds the store; it renders
 * nothing and never unmounts for the life of the app.
 */
export const AgentRunEventListener: React.FC = () => {
  const appendEvent = useAgentRunEventsStore((state) => state.appendEvent);

  useEffect(() => {
    const unlisten = events.agentRunEvent.listen((event) => {
      const { run_id, event: runEvent } = event.payload;
      appendEvent(run_id, runEvent);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [appendEvent]);

  return null;
};
