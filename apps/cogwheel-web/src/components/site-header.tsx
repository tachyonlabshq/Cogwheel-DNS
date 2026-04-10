import { useLocation } from "react-router-dom";
import { Separator } from "@/components/ui/separator";
import { SidebarTrigger } from "@/components/ui/sidebar";

const pageNames: Record<string, string> = {
  "/": "Dashboard",
  "/profiles": "Block Profiles",
  "/devices": "Devices",
  "/grease-ai": "Grease-AI",
  "/settings": "Settings",
};

export function SiteHeader() {
  const location = useLocation();
  const pageName = pageNames[location.pathname] ?? "Dashboard";

  return (
    <header className="flex h-12 shrink-0 items-center gap-2 border-b px-4 transition-[width,height] ease-linear group-has-data-[collapsible=icon]/sidebar-wrapper:h-12">
      <div className="flex items-center gap-2">
        <SidebarTrigger className="-ml-1" />
        <Separator
          orientation="vertical"
          className="mr-2 data-[orientation=vertical]:h-4"
        />
        <span className="text-sm font-medium">{pageName}</span>
      </div>
    </header>
  );
}
