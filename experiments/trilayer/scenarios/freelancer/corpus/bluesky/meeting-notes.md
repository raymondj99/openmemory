# BlueSky Dental — meeting notes

## Kickoff, 2026-05-12
Attendees: Dr. Okafor, practice manager Lena, me. Decision: build
reschedule flow before new-patient flow; it is 70% of call volume.

## API review, 2026-06-09
Their vendor's slot API cannot hold a tentative booking. Workaround:
optimistic lock client-side, confirm within 90 seconds. Lena approved
the compromise.
