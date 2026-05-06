import { User, Bell, Shield, HelpCircle, LogOut, ChevronRight, Moon, Sun, Monitor } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

interface SettingsSectionProps {
  title: string
  children: React.ReactNode
}

function SettingsSection({ title, children }: SettingsSectionProps) {
  return (
    <div className="bg-white rounded-xl shadow-sm overflow-hidden mb-6">
      <div className="p-4 border-b bg-gray-50/50">
        <h2 className="font-heading font-semibold text-foreground">{title}</h2>
      </div>
      <div className="p-4">{children}</div>
    </div>
  )
}

interface SettingsItemProps {
  icon: React.ReactNode
  label: string
  value?: string
  action?: React.ReactNode
  onClick?: () => void
}

function SettingsItem({ icon, label, value, action, onClick }: SettingsItemProps) {
  return (
    <button
      onClick={onClick}
      className={cn(
        'flex items-center gap-4 w-full p-3 rounded-lg transition-colors',
        onClick && 'hover:bg-gray-50'
      )}
    >
      <div className="p-2 bg-gray-100 rounded-lg text-gray-600">
        {icon}
      </div>
      <div className="flex-1 text-left">
        <p className="font-medium text-foreground">{label}</p>
        {value && <p className="text-sm text-muted-foreground">{value}</p>}
      </div>
      {action || (onClick && <ChevronRight className="h-5 w-5 text-gray-400" />)}
    </button>
  )
}

export function SettingsComponent() {
  return (
    <div className="min-h-screen bg-cream">
      <div className="max-w-3xl mx-auto px-6 py-8">
        {/* Header */}
        <header className="mb-8">
          <h1 className="font-heading text-3xl font-bold text-foreground mb-2">
            Settings
          </h1>
          <p className="text-muted-foreground">
            Manage your account and preferences
          </p>
        </header>

        {/* Profile Card */}
        <div className="bg-white rounded-xl p-6 shadow-sm mb-6">
          <div className="flex items-center gap-4">
            <div className="h-16 w-16 rounded-full bg-sanctuary-green flex items-center justify-center text-white text-xl font-bold">
              JD
            </div>
            <div className="flex-1">
              <h2 className="font-heading text-lg font-semibold text-foreground">
                John Doe
              </h2>
              <p className="text-sm text-muted-foreground">john.doe@example.com</p>
            </div>
            <Button variant="outline" size="sm">
              Edit Profile
            </Button>
          </div>
        </div>

        {/* Account */}
        <SettingsSection title="Account">
          <div className="space-y-1">
            <SettingsItem
              icon={<User className="h-5 w-5" />}
              label="Personal Information"
              onClick={() => {}}
            />
            <SettingsItem
              icon={<Shield className="h-5 w-5" />}
              label="Security"
              value="2FA enabled"
              onClick={() => {}}
            />
            <SettingsItem
              icon={<Bell className="h-5 w-5" />}
              label="Notifications"
              value="Push, Email"
              onClick={() => {}}
            />
          </div>
        </SettingsSection>

        {/* Appearance */}
        <SettingsSection title="Appearance">
          <div className="space-y-4">
            <div>
              <p className="text-sm font-medium text-foreground mb-3">Theme</p>
              <div className="flex gap-3">
                <button className="flex items-center gap-2 px-4 py-2 rounded-lg bg-sanctuary-green/10 text-sanctuary-green border-2 border-sanctuary-green">
                  <Sun className="h-4 w-4" />
                  <span className="text-sm font-medium">Light</span>
                </button>
                <button className="flex items-center gap-2 px-4 py-2 rounded-lg bg-gray-100 text-gray-600 border-2 border-transparent">
                  <Moon className="h-4 w-4" />
                  <span className="text-sm font-medium">Dark</span>
                </button>
                <button className="flex items-center gap-2 px-4 py-2 rounded-lg bg-gray-100 text-gray-600 border-2 border-transparent">
                  <Monitor className="h-4 w-4" />
                  <span className="text-sm font-medium">System</span>
                </button>
              </div>
            </div>
            
            <div>
              <p className="text-sm font-medium text-foreground mb-3">Accent Color</p>
              <div className="flex gap-3">
                <button className="h-10 w-10 rounded-full bg-sanctuary-green ring-2 ring-offset-2 ring-sanctuary-green" />
                <button className="h-10 w-10 rounded-full bg-work-blue" />
                <button className="h-10 w-10 rounded-full bg-gym-terracotta" />
                <button className="h-10 w-10 rounded-full bg-family-plum" />
              </div>
            </div>
          </div>
        </SettingsSection>

        {/* Integrations */}
        <SettingsSection title="Integrations">
          <div className="space-y-1">
            <SettingsItem
              icon={<svg className="h-5 w-5" viewBox="0 0 24 24"><path fill="currentColor" d="M12 0C5.4 0 0 5.4 0 12s5.4 12 12 12 12-5.4 12-12S18.6 0 12 0zm5.4 16.2L16.2 18l-4.2-4.2-4.2 4.2-1.2-1.8 4.2-4.2-4.2-4.2 1.2-1.8 4.2 4.2 4.2-4.2 1.2 1.8-4.2 4.2 4.2 4.2z"/></svg>}
              label="Google Calendar"
              value="Connected"
              action={<span className="text-sm text-green-600 font-medium">Connected</span>}
            />
          </div>
        </SettingsSection>

        {/* Support */}
        <SettingsSection title="Support">
          <div className="space-y-1">
            <SettingsItem
              icon={<HelpCircle className="h-5 w-5" />}
              label="Help Center"
              onClick={() => {}}
            />
            <SettingsItem
              icon={<svg className="h-5 w-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M4 4h16c1.1 0 2 .9 2 2v12c0 1.1-.9 2-2 2H4c-1.1 0-2-.9-2-2V6c0-1.1.9-2 2-2z"/><polyline points="22,6 12,13 2,6"/></svg>}
              label="Contact Support"
              onClick={() => {}}
            />
          </div>
        </SettingsSection>

        {/* Sign Out */}
        <Button 
          variant="outline" 
          className="w-full text-red-600 border-red-200 hover:bg-red-50"
        >
          <LogOut className="h-4 w-4 mr-2" />
          Sign Out
        </Button>

        <p className="text-center text-xs text-muted-foreground mt-8">
          Sanctuary v0.1.0 · Made with care
        </p>
      </div>
    </div>
  )
}
