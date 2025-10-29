# /// script
# dependencies = [
#   "boto3>=1.35.0",
# ]
# ///

"""
Update `metadata.mirrors` and `primary_mirror` fields in the `sc-dev-products` DynamoDB table.

If any record uses connection_id == "aws-opendata-us-west-2",
it will be replaced with "aws-opendata-us-west-2-prod" in:
  - the mirror key
  - mirror.config.bucket
  - mirror.connection_id
  - metadata.primary_mirror

Usage:
    uv run update_sc_dev_products.py
"""

import boto3
from datetime import datetime
from botocore.exceptions import ClientError

# --- Configuration ---
TABLE_NAME = "sc-dev-products"
OLD_CONN = "aws-opendata-us-west-2"
NEW_CONN = "aws-opendata-us-west-2-prod"

dynamodb = boto3.resource("dynamodb")
table = dynamodb.Table(TABLE_NAME)


def update_metadata(metadata: dict) -> dict:
    """Return updated metadata if OLD_CONN is present."""
    if not metadata or "mirrors" not in metadata:
        return metadata

    new_mirrors = {}
    modified = False

    for conn_id, mirror in metadata["mirrors"].items():
        if conn_id == OLD_CONN or mirror.get("connection_id") == OLD_CONN:
            new_mirror = mirror.copy()
            new_mirror["connection_id"] = NEW_CONN
            if "config" in new_mirror:
                new_mirror["config"]["bucket"] = NEW_CONN
            new_mirrors[NEW_CONN] = new_mirror
            modified = True
        else:
            new_mirrors[conn_id] = mirror

    if modified:
        metadata["mirrors"] = new_mirrors
        if metadata.get("primary_mirror") == OLD_CONN:
            metadata["primary_mirror"] = NEW_CONN

    return metadata


def process_all_records():
    """Scan the entire table and update matching records."""
    scan_kwargs = {}
    total_updated = 0
    total_scanned = 0

    print(f"Scanning table '{TABLE_NAME}'...")

    while True:
        response = table.scan(**scan_kwargs)
        items = response.get("Items", [])
        total_scanned += len(items)

        for item in items:
            metadata = item.get("metadata", {})
            updated_metadata = update_metadata(metadata)

            if metadata == update_metadata:
                print(f"Item unchanged: {item['account_id']}/{item['product_id']}")

            # Only update if there was a change
            total_updated += 1
            item_key = {
                "account_id": item["account_id"],
                "product_id": item["product_id"],
            }

            try:
                table.update_item(
                    Key=item_key,
                    UpdateExpression="SET metadata = :m",
                    ExpressionAttributeValues={
                        ":m": updated_metadata,
                    },
                )
                print(f"✅ Updated {item_key}")
            except ClientError as e:
                print(f"⚠️ Failed to update {item_key}: {e}")

        # Handle pagination
        last_key = response.get("LastEvaluatedKey")
        if not last_key:
            break
        scan_kwargs["ExclusiveStartKey"] = last_key

    print(f"\nScan complete: {total_scanned} records scanned, {total_updated} updated.")


if __name__ == "__main__":
    process_all_records()
