import React from "react";
import { useTranslation } from "react-i18next";
import { SttSetupCard } from "./SttSetupCard";
import { CleanupSetupCard } from "./CleanupSetupCard";

export const ModelSetupSettings: React.FC = () => {
  const { t } = useTranslation();

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="mb-2">
        <h1 className="text-xl font-semibold mb-2">
          {t("settings.modelSetup.title")}
        </h1>
        <p className="text-sm text-text/60">
          {t("settings.modelSetup.description")}
        </p>
      </div>

      <SttSetupCard />
      <CleanupSetupCard />
    </div>
  );
};
