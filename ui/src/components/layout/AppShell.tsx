// AppShell \u2014 the chrome that wraps every authenticated route.
// Layout grid:
//   row 1: TopBar (h-12)
//   row 2: Sidebar (fixed 240px) + main outlet (flex-1)

import { Outlet } from "@tanstack/react-router";

import { Sidebar } from "@/components/layout/Sidebar";
import { TopBar } from "@/components/layout/TopBar";

export function AppShell() {
  return (
    <div className="flex h-screen flex-col bg-background">
      <TopBar />
      <div className="flex flex-1 overflow-hidden">
        <Sidebar />
        <main className="flex-1 overflow-auto p-6">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
