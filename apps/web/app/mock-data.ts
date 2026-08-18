import type { List, TimeBlock, TaskEvent, TimelineItem } from './types';

export const lists: List[] = [
  { id: 'work', name: 'Work', color: '#2a5c8a' },
  { id: 'gym', name: 'Fitness', color: '#c45a2c' },
  { id: 'family', name: 'Family', color: '#7a4a6a' },
  { id: 'personal', name: 'Personal', color: '#3a7a5a' },
];

export const todayItems: TimelineItem[] = [
  // Standalone tasks (no list)
  {
    id: 'task-morning-1',
    title: 'Fresh Up',
    duration: 20,
    priority: 'high',
    startTime: '6:00 AM',
    endTime: '6:20 AM',
  } as TaskEvent,
  {
    id: 'task-morning-2',
    title: 'Breakfast',
    duration: 30,
    priority: 'medium',
    startTime: '6:20 AM',
    endTime: '6:50 AM',
  } as TaskEvent,
  {
    id: 'task-morning-3',
    title: 'Dish Washing',
    duration: 30,
    priority: 'low',
    startTime: '7:00 AM',
    endTime: '7:30 AM',
  } as TaskEvent,
  {
    id: 'task-morning-4',
    title: 'Cleaning Kitchen',
    duration: 30,
    priority: 'medium',
    startTime: '7:00 AM',
    endTime: '7:30 AM',
  } as TaskEvent,
  {
    id: 'task-morning-5',
    title: 'Pack Lunch',
    duration: 15,
    priority: 'medium',
    startTime: '7:30 AM',
    endTime: '7:45 AM',
  } as TaskEvent,
  {
    id: 'task-morning-6',
    title: 'Commute',
    duration: 15,
    priority: 'high',
    startTime: '7:45 AM',
    endTime: '8:00 AM',
  } as TaskEvent,
  // Time blocks (with lists)
  {
    id: 'block-1',
    listId: 'work',
    listName: 'Work',
    listColor: '#2a5c8a',
    startTime: '9:00 AM',
    endTime: '12:00 PM',
    tasks: [
      { id: 't1', title: 'Review Q3 Roadmap', duration: 30, priority: 'high' },
      { id: 't2', title: 'Team Standup', duration: 45, priority: 'high' },
      {
        id: 't3',
        title: 'Update Dashboard Mockups',
        duration: 60,
        priority: 'medium',
      },
      {
        id: 't4',
        title: 'Design API Architecture',
        duration: 90,
        priority: 'high',
      },
      {
        id: 't5',
        title: 'Review Frontend PRs',
        duration: 30,
        priority: 'medium',
      },
      { id: 't6', title: 'Write Documentation', duration: 45, priority: 'low' },
      {
        id: 't7',
        title: 'Client Meeting Prep',
        duration: 20,
        priority: 'high',
      },
      { id: 't8', title: 'Email Cleanup', duration: 15, priority: 'low' },
    ],
  } as TimeBlock,
  {
    id: 'block-2',
    listId: 'gym',
    listName: 'Gym Time',
    listColor: '#c45a2c',
    startTime: '12:00 PM',
    endTime: '2:00 PM',
    tasks: [
      { id: 't9', title: 'Strength Training', duration: 90, priority: 'high' },
    ],
  } as TimeBlock,
  {
    id: 'block-3',
    listId: 'family',
    listName: 'Family Dinner',
    listColor: '#7a4a6a',
    startTime: '2:00 PM',
    endTime: '4:00 PM',
    tasks: [
      { id: 't10', title: 'Pick up groceries', duration: 30, priority: 'high' },
      { id: 't11', title: 'Cook dinner', duration: 60, priority: 'medium' },
    ],
  } as TimeBlock,
  {
    id: 'block-4',
    listId: 'personal',
    listName: 'Evening Relaxation',
    listColor: '#3a7a5a',
    startTime: '4:00 PM',
    endTime: '6:00 PM',
    tasks: [
      { id: 't12', title: 'Reading', duration: 45, priority: 'medium' },
      { id: 't13', title: 'Meditation', duration: 20, priority: 'low' },
    ],
  } as TimeBlock,
];

// Backwards compatibility alias
export const todayBlocks = todayItems;

export const quotes = [
  {
    text: 'The only way to do great work is to love what you do.',
    author: 'Steve Jobs',
  },
  { text: 'Focus on being productive instead of busy.', author: 'Tim Ferriss' },
  {
    text: 'Your future is created by what you do today, not tomorrow.',
    author: 'Robert Kiyosaki',
  },
];
