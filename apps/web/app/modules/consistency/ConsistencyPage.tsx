import { Flame, Target, Calendar, Clock, BookOpen, Dumbbell, Moon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

interface Rule {
  id: string
  name: string
  icon: React.ReactNode
  condition: string
  schedule: string
  streak: number
  longestStreak: number
  compliance: number
  history: boolean[]
}

const rules: Rule[] = [
  {
    id: '1',
    name: 'Wake up early',
    icon: <Clock className="h-5 w-5" />,
    condition: 'Wake up between 5:00-6:00 AM',
    schedule: 'Daily',
    streak: 12,
    longestStreak: 23,
    compliance: 82,
    history: [true, true, true, false, true, true, true, true, true, false, true, true, true, true],
  },
  {
    id: '2',
    name: 'Morning run',
    icon: <Dumbbell className="h-5 w-5" />,
    condition: 'Run or workout before 8 AM',
    schedule: 'Weekdays',
    streak: 5,
    longestStreak: 15,
    compliance: 68,
    history: [true, false, true, true, true, false, true, true, false, true, true, true, false, true],
  },
  {
    id: '3',
    name: 'No work after 8 PM',
    icon: <Moon className="h-5 w-5" />,
    condition: 'No work events after 8:00 PM',
    schedule: 'Daily',
    streak: 8,
    longestStreak: 12,
    compliance: 75,
    history: [true, true, false, true, true, true, true, true, false, true, true, true, true, false],
  },
  {
    id: '4',
    name: 'Read before bed',
    icon: <BookOpen className="h-5 w-5" />,
    condition: 'Reading block exists after 9 PM',
    schedule: 'Daily',
    streak: 3,
    longestStreak: 8,
    compliance: 45,
    history: [true, false, false, true, true, false, true, false, true, true, false, false, true, false],
  },
]

function RuleCard({ rule }: { rule: Rule }) {
  return (
    <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
      <div className="flex items-start justify-between mb-4">
        <div className="flex items-center gap-3">
          <div className="p-2 bg-primary/10 rounded-lg text-primary">
            {rule.icon}
          </div>
          <div>
            <h3 className="font-heading font-semibold text-foreground">{rule.name}</h3>
            <p className="text-sm text-muted-foreground">{rule.condition}</p>
          </div>
        </div>
        <span className="text-xs px-2 py-1 bg-muted rounded-full text-muted-foreground">
          {rule.schedule}
        </span>
      </div>

      {/* Stats */}
      <div className="grid grid-cols-3 gap-4 mb-4">
        <div className="text-center">
          <div className="flex items-center justify-center gap-1 text-gym-terracotta mb-1">
            <Flame className="h-4 w-4" />
            <span className="font-bold text-lg">{rule.streak}</span>
          </div>
          <p className="text-xs text-muted-foreground">Current streak</p>
        </div>
        <div className="text-center">
          <div className="flex items-center justify-center gap-1 text-primary mb-1">
            <Target className="h-4 w-4" />
            <span className="font-bold text-lg">{rule.compliance}%</span>
          </div>
          <p className="text-xs text-muted-foreground">This month</p>
        </div>
        <div className="text-center">
          <div className="flex items-center justify-center gap-1 text-work-blue mb-1">
            <Calendar className="h-4 w-4" />
            <span className="font-bold text-lg">{rule.longestStreak}</span>
          </div>
          <p className="text-xs text-muted-foreground">Best streak</p>
        </div>
      </div>

      {/* History Heatmap */}
      <div className="flex items-center gap-1">
        <span className="text-xs text-muted-foreground mr-2">14 days:</span>
        {rule.history.map((met, i) => (
          <div
            key={i}
            className={cn(
              'h-6 w-6 rounded',
              met ? 'bg-primary' : 'bg-destructive/20'
            )}
          />
        ))}
      </div>
    </div>
  )
}

export function ConsistencyPage() {
  return (
    <div className="min-h-screen bg-cream">
      <div className="max-w-7xl mx-auto px-6 py-8">
        {/* Header */}
        <header className="flex items-center justify-between mb-8">
          <div>
            <h1 className="font-heading text-3xl font-bold text-foreground mb-2">
              Consistency
            </h1>
            <p className="text-muted-foreground">
              Track your personal commitments and build better habits
            </p>
          </div>
          <Button className="bg-primary hover:bg-primary/90">
            <Target className="h-4 w-4 mr-2" />
            New Rule
          </Button>
        </header>

        {/* Overall Stats */}
        <div className="grid grid-cols-1 md:grid-cols-4 gap-6 mb-8">
          <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
            <p className="text-3xl font-bold text-foreground">28</p>
            <p className="text-sm text-muted-foreground">Day streak</p>
          </div>
          <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
            <p className="text-3xl font-bold text-primary">72%</p>
            <p className="text-sm text-muted-foreground">Avg compliance</p>
          </div>
          <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
            <p className="text-3xl font-bold text-work-blue">4</p>
            <p className="text-sm text-muted-foreground">Active rules</p>
          </div>
          <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
            <p className="text-3xl font-bold text-gym-terracotta">156</p>
            <p className="text-sm text-muted-foreground">Total checks</p>
          </div>
        </div>

        {/* Rules Grid */}
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          {rules.map((rule) => (
            <RuleCard key={rule.id} rule={rule} />
          ))}
        </div>
      </div>
    </div>
  )
}
