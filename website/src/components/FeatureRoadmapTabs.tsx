import { useState } from "react";
import type { Messages } from "@/lib/locales";
import { motion } from "framer-motion";
import { Map, ThumbsUp } from "lucide-react";
import { useLocale } from "@/lib/i18n";
import RoadmapSection from "./RoadmapSection";
import FeatureVotePage from "./FeatureVotePage";

type TabKey = "roadmap" | "vote";

export default function FeatureRoadmapTabs() {
  const { t } = useLocale();
  const [activeTab, setActiveTab] = useState<TabKey>("roadmap");

  const tabs: { key: TabKey; icon: typeof Map; labelKey: keyof Messages }[] = [
    { key: "roadmap", icon: Map, labelKey: "roadmap.tabLabel" },
    { key: "vote", icon: ThumbsUp, labelKey: "featureVote.tabLabel" },
  ];

  return (
    <>
      {/* Tab switcher */}
      <div className="pt-24 pb-0">
        <div className="mx-auto max-w-4xl px-4 sm:px-6 lg:px-8">
          <div className="flex items-center justify-center">
            <div className="inline-flex items-center gap-1 p-1 rounded-lg bg-dark-surface1 border border-dark-border">
              {tabs.map(({ key, icon: Icon, labelKey }) => (
                <button
                  key={key}
                  onClick={() => setActiveTab(key)}
                  className={`relative flex items-center gap-1.5 px-4 py-2 rounded-md text-sm font-medium transition-all duration-200 cursor-pointer ${
                    activeTab === key
                      ? "text-dark-text"
                      : "text-dark-text-secondary hover:text-dark-text-muted"
                  }`}
                >
                  <Icon className="w-4 h-4" />
                  {t(labelKey)}
                  {activeTab === key && (
                    <motion.div
                      layoutId="feature-roadmap-tab-bg"
                      className="absolute inset-0 rounded-md bg-dark-surface2 border border-dark-border -z-10"
                      transition={{ type: "spring", bounce: 0.15, duration: 0.4 }}
                    />
                  )}
                </button>
              ))}
            </div>
          </div>
        </div>
      </div>

      {/* Tab content */}
      {activeTab === "roadmap" ? <RoadmapSection /> : <FeatureVotePage />}
    </>
  );
}
