# Create accounts directory if it doesn't exist
mkdir -p accounts

# Remove existing account files if any
rm -f accounts/* 2>/dev/null || true

npx ts-node new_scrape.ts &&
python setup_validator.py &&
bash start_localnet.sh