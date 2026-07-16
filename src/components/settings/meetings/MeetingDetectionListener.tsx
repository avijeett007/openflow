import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands, events } from "@/bindings";
import { useNavigationStore } from "../../../stores/navigationStore";

/**
 * Global listener for the `meeting-detected` event. When a known meeting app is
 * running and the mic is in use, OpenFlow offers a non-intrusive prompt to start
 * capturing — regardless of which settings section is open. Capture start is
 * always user-confirmed (the toast action); detection never records on its own
 * (DESIGN-meetings.md §3). Mounted once at the App root.
 */
export const MeetingDetectionListener: React.FC = () => {
  const { t } = useTranslation();
  const setCurrentSection = useNavigationStore(
    (state) => state.setCurrentSection,
  );

  useEffect(() => {
    const unlisten = events.meetingDetected.listen((event) => {
      const { bundle_id, app_name } = event.payload;
      toast(t("settings.meetings.detectedTitle", { app: app_name }), {
        description: t("settings.meetings.detectedBody"),
        duration: 12000,
        action: {
          label: t("settings.meetings.startCapture"),
          onClick: () => {
            void commands.startMeetingCapture(bundle_id).then((result) => {
              if (result.status === "error") {
                toast.error(
                  t("settings.meetings.startError", { error: result.error }),
                );
              } else {
                setCurrentSection("meetings");
              }
            });
          },
        },
      });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t, setCurrentSection]);

  return null;
};
