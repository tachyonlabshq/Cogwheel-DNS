import { Outlet } from "react-router-dom";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import { AppSidebar } from "@/components/app-sidebar";
import { Toaster } from "@/components/ui/sonner";
import { CogwheelProvider } from "@/contexts/cogwheel-context";

export function AppLayout() {
  return (
    <CogwheelProvider>
      <SidebarProvider>
        <AppSidebar />
        <SidebarInset>
          <div className="flex flex-1 flex-col">
            <div className="flex flex-1 flex-col gap-4 p-4 md:gap-6 md:p-8 lg:p-10">
              <Outlet />
            </div>
          </div>
        </SidebarInset>
      </SidebarProvider>
      <Toaster />
    </CogwheelProvider>
  );
}
