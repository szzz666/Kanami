import { Modal, List, Button, Space, Typography, Tag } from "antd";
import { CheckOutlined, CloseOutlined } from "@ant-design/icons";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";

/// 接入确认弹窗。
///
/// 按共识：首次接入需播放端（这里是音源端）点一次「允许」，
/// 之后记住该设备。不做这一步，同网段任何人都能把声音推进来。
export default function PendingModal({ k }: { k: KanamiApi }) {
  const open = k.pending.length > 0;

  return (
    <Modal
      open={open}
      title={S.pendingTitle}
      footer={null}
      closable={false}
      maskClosable={false}
    >
      <Typography.Paragraph type="secondary">{S.pendingHint}</Typography.Paragraph>
      <List
        dataSource={k.pending}
        renderItem={(req) => (
          <List.Item
            actions={[
              <Button
                key="ok"
                type="primary"
                icon={<CheckOutlined />}
                onClick={() => k.approvePeer(req.device_id)}
              >
                {S.pendingApprove}
              </Button>,
              <Button
                key="no"
                danger
                icon={<CloseOutlined />}
                onClick={() => k.rejectPeer(req.device_id)}
              >
                {S.pendingReject}
              </Button>,
            ]}
          >
            <List.Item.Meta
              title={req.name}
              description={
                <Space>
                  <Tag>{req.addr}</Tag>
                </Space>
              }
            />
          </List.Item>
        )}
      />
    </Modal>
  );
}
